//! `solidcraft-agent`: a command-line tool built for AI agents to drive the running SolidCraft app.
//!
//! Every invocation is one short-lived process: it finds the app (its discovery file, or `--port`),
//! connects to its loopback control channel with the app's token (docs/control-protocol.md), runs one
//! tool, prints compact JSON and exits. Nothing stays running between calls: the app holds the design,
//! its undo history and the window the person watches. The tools are the MCP server's own
//! (`solidcraft_mcp::call_tool_with`), so `batch` rolls back and checks `expect` exactly as it does over
//! MCP, without an MCP server.
//!
//! Two ways to work, on the same open design:
//! - small tweaks: `solidcraft-agent exec timeline.edit '{...}'`, `solidcraft-agent undo`, `measure`
//! - whole builds: `solidcraft-agent batch build.json`, which by default prints only a summary.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    agent::main()
}

#[cfg(not(target_arch = "wasm32"))]
mod agent {
    use std::io::Read as _;
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use serde_json::{Map, Value, json};
    use solidcraft_engine::control_auth::APP;
    use solidcraft_mcp::{Backend as _, Options, Remote};

    const USAGE: &str = "\
solidcraft-agent: drive the running SolidCraft app, one call per invocation (compact JSON on stdout)

usage:
  solidcraft-agent launch [--wait SECS] [--app PATH]   connect to the open app, or open it and wait until it answers
  solidcraft-agent status                              is the app up: port, version, open designs
  solidcraft-agent exec <command> [PARAMS] [--expect JSON]
                                                       one engine command (small tweaks), e.g.
                                                       exec solid.fillet '{\"edges\":[[0,0,10]],\"radius\":3}'
  solidcraft-agent batch <FILE|-> [--expect JSON] [--results summary|errors|all] [--no-rollback] [--timings]
                                                       a whole build in one call; all-or-nothing by default.
                                                       FILE holds [{command, params, echo?}, ...] or {commands: [...], expect?, ...}.
                                                       --results summary (default): ok, counts, failure, expectation, and the
                                                       results of steps marked \"echo\": true only; errors: also the failed step;
                                                       all: every step's result. Each step is its own undo step:
                                                       `edit.checkpoint` / `edit.restore_checkpoint` take a whole batch back
  solidcraft-agent screenshot [ARGS] [--out FILE.png]  render or capture; prints the PNG's path (default: a temp file)
  solidcraft-agent <tool> [ARGS]                       any tool the MCP server has (measure, inspect_design, body_topology,
                                                       undo, redo, save, export, open, new_design, set_parameter,
                                                       check_parameters, list_commands, describe_command, skill_reference, ...)
  solidcraft-agent tools                               every tool's name and summary
  solidcraft-agent help <tool>                         one tool's description and argument schema

ARGS / PARAMS: inline JSON, @file.json, or - (stdin). Relative file paths in them are made absolute here.

options (anywhere):
  --port PORT | --connect HOST:PORT   which app (default: the most recently started one, from its discovery file)
  --token T                           the control token (default: SOLIDCRAFT_CONTROL_TOKEN, else the app's token file)
  --param-check off|warn|strict       check calls against their commands' contracts
  --selection                         add the person's current selection in the window to the reply
  --pretty                            indented JSON

exit status: 0 ok, 1 the tool reported an error, 2 bad usage, 3 no app is running (run `solidcraft-agent launch`)";

    const EXIT_TOOL: u8 = 1;
    const EXIT_USAGE: u8 = 2;
    const EXIT_NO_APP: u8 = 3;
    /// How a tool error starts when nothing listens where the app should be (`backend::NOT_RUNNING`).
    const NOT_RUNNING: &str = "no SolidCraft app is listening at";

    /// A failure: its exit status and the message printed as `{"ok": false, "error": …}`.
    struct Fail(u8, String);

    fn usage_err(msg: impl Into<String>) -> Fail {
        Fail(EXIT_USAGE, msg.into())
    }

    /// The command line, split into positional words and the options any command takes.
    struct Cli {
        words: Vec<String>,
        flags: Map<String, Value>,
    }

    /// Options that take a value; every other `--word` is a switch.
    const VALUED: &[&str] =
        &["--port", "--connect", "--token", "--param-check", "--expect", "--results", "--out", "--wait", "--app"];

    impl Cli {
        fn parse(args: Vec<String>) -> Result<Cli, Fail> {
            let mut words = Vec::new();
            let mut flags = Map::new();
            let mut it = args.into_iter();
            while let Some(a) = it.next() {
                if a == "-" || !a.starts_with("--") {
                    words.push(a);
                } else if VALUED.contains(&a.as_str()) {
                    let v = it.next().ok_or_else(|| usage_err(format!("{a} needs a value")))?;
                    flags.insert(a, Value::String(v));
                } else if let Some((k, v)) = a.split_once('=').filter(|(k, _)| VALUED.contains(k)) {
                    flags.insert(k.to_string(), Value::String(v.to_string()));
                } else {
                    flags.insert(a, Value::Bool(true));
                }
            }
            Ok(Cli { words, flags })
        }
        fn get(&self, k: &str) -> Option<&str> {
            self.flags.get(k).and_then(Value::as_str)
        }
        fn has(&self, k: &str) -> bool {
            self.flags.contains_key(k)
        }
    }

    pub fn main() -> ExitCode {
        let cli = match Cli::parse(std::env::args().skip(1).collect()) {
            Ok(c) => c,
            Err(f) => return fail(f, false),
        };
        let pretty = cli.has("--pretty");
        match run(&cli) {
            Ok(v) => {
                print_json(&v, pretty);
                ExitCode::SUCCESS
            }
            Err(f) => fail(f, pretty),
        }
    }

    fn fail(Fail(code, msg): Fail, pretty: bool) -> ExitCode {
        if code == EXIT_USAGE && msg.is_empty() {
            eprintln!("{USAGE}");
        } else {
            // A structured failure (a batch's summary) prints as itself, not as a string inside `error`.
            match serde_json::from_str::<Value>(&msg) {
                Ok(Value::Object(mut o)) => {
                    o.insert("ok".into(), json!(false));
                    print_json(&Value::Object(o), pretty);
                }
                _ => print_json(&json!({"ok": false, "error": msg}), pretty),
            }
            if code == EXIT_USAGE {
                eprintln!("(solidcraft-agent --help for usage)");
            }
        }
        ExitCode::from(code)
    }

    fn print_json(v: &Value, pretty: bool) {
        use std::io::Write as _;
        let text = if pretty { serde_json::to_string_pretty(v) } else { serde_json::to_string(v) }.unwrap_or_default();
        // Written, not println!ed: a closed pipe (`| head`) is not a crash.
        let _ = writeln!(std::io::stdout().lock(), "{text}");
    }

    fn run(cli: &Cli) -> Result<Value, Fail> {
        let Some(cmd) = cli.words.first().map(String::as_str) else { return Err(usage_err("")) };
        let rest = &cli.words[1..];
        match cmd {
            "help" | "-h" if rest.is_empty() => Err(usage_err("")),
            "help" => tool_help(&rest[0]),
            "tools" => Ok(tool_list()),
            "launch" => launch(cli),
            "status" => status(cli),
            "exec" => {
                let command = rest.first().ok_or_else(|| usage_err("exec: which command? e.g. exec solid.box '{\"length\":10,...}'"))?;
                let params = match rest.get(1) {
                    Some(p) => read_json(p)?,
                    None => json!({}),
                };
                let mut args = json!({"command": command, "params": absolutize_step_paths(command, params), "verbose": false});
                if let Some(e) = cli.get("--expect") {
                    args["expect"] = read_json(e)?;
                }
                call(cli, "execute", args)
            }
            "batch" => batch(cli, rest),
            "screenshot" => screenshot(cli, rest),
            tool => {
                if !tool_names().iter().any(|t| t == tool) {
                    return Err(usage_err(format!("unknown command or tool `{tool}`; `solidcraft-agent tools` lists the tools")));
                }
                let args = match rest.first() {
                    Some(a) => read_json(a)?,
                    None => json!({}),
                };
                let mut args = absolutize_tool_paths(tool, args);
                if let Some(e) = cli.get("--expect")
                    && let Some(o) = args.as_object_mut()
                {
                    o.insert("expect".into(), read_json(e)?);
                }
                call(cli, tool, args)
            }
        }
    }

    // ---- reaching the app ---------------------------------------------------------------------

    /// The address of the app this call is for: `--connect`, `--port`, else the discovery file of the
    /// most recently started app, if something listens there.
    fn app_addr(cli: &Cli) -> Result<String, Fail> {
        if let Some(a) = cli.get("--connect") {
            return Ok(a.to_string());
        }
        if let Some(p) = cli.get("--port") {
            let port: u16 = p.parse().map_err(|_| usage_err(format!("--port wants a number, not `{p}`")))?;
            return Ok(format!("127.0.0.1:{port}"));
        }
        discover().map(|p| format!("127.0.0.1:{p}")).ok_or_else(|| {
            Fail(EXIT_NO_APP, "no SolidCraft app is running (no listening discovery file); run `solidcraft-agent launch`".into())
        })
    }

    fn listening(port: u16) -> bool {
        TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(300)).is_ok()
    }

    /// The port the most recently started app wrote to its discovery file, when it still listens.
    fn discover() -> Option<u16> {
        APP.read_current_port().filter(|p| listening(*p))
    }

    fn port_of(addr: &str) -> Option<u16> {
        addr.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
    }

    fn remote(cli: &Cli) -> Result<Remote, Fail> {
        let addr = app_addr(cli)?;
        let level = match cli.get("--param-check") {
            None => solidcraft_engine::schema::ParamCheck::Off,
            Some(l) => solidcraft_engine::schema::ParamCheck::parse(l)
                .ok_or_else(|| usage_err(format!("--param-check is off, warn or strict, not `{l}`")))?,
        };
        Ok(Remote::new(&addr, cli.get("--token")).with_param_check(level))
    }

    /// Run one MCP tool against the app and turn its result into this tool's output.
    fn call(cli: &Cli, tool: &str, args: Value) -> Result<Value, Fail> {
        let mut r = remote(cli)?;
        let opts = Options { compact: true, ..Options::default() };
        let result = solidcraft_mcp::call_tool_with(&mut r, tool, &args, opts);
        let mut out = unwrap_result(&result)?;
        if cli.has("--selection")
            && let Ok(sel) = r.call("document.selection", json!({}))
            && let (Some(s), Some(o)) = (sel.get("selection").filter(|s| !s.is_null()), out.as_object_mut())
        {
            o.insert("selection".into(), s.clone());
        }
        Ok(out)
    }

    /// An MCP tool result (`{content: [...], isError}`) as plain JSON: its text parsed when it is JSON,
    /// pictures written to files. An error result is a `Fail`.
    fn unwrap_result(result: &Value) -> Result<Value, Fail> {
        let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let mut texts: Vec<Value> = Vec::new();
        let mut images: Vec<String> = Vec::new();
        for block in result.get("content").and_then(Value::as_array).into_iter().flatten() {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let t = block.get("text").and_then(Value::as_str).unwrap_or_default();
                    texts.push(serde_json::from_str::<Value>(t).unwrap_or_else(|_| Value::String(t.to_string())));
                }
                Some("image") => {
                    if let Some(p) = block.get("data").and_then(Value::as_str).and_then(save_image) {
                        images.push(p);
                    }
                }
                _ => {}
            }
        }
        if is_error {
            let msg = texts
                .iter()
                .map(|t| match t {
                    Value::String(s) => s.clone(),
                    v => v.to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            let code = if msg.contains(NOT_RUNNING) { EXIT_NO_APP } else { EXIT_TOOL };
            // A batch that failed still has useful structure: keep it.
            if let [v @ Value::Object(_)] = texts.as_slice() {
                return Err(Fail(code, v.to_string()));
            }
            return Err(Fail(code, msg));
        }
        let mut out = match texts.len() {
            0 => json!({}),
            1 => texts.remove(0),
            _ => Value::Array(texts),
        };
        if !images.is_empty() {
            if !out.is_object() {
                out = json!({"result": out});
            }
            if let Some(o) = out.as_object_mut() {
                o.insert("image".into(), json!(images.first()));
                if images.len() > 1 {
                    o.insert("images".into(), json!(images));
                }
            }
        }
        Ok(out)
    }

    // ---- launch and status --------------------------------------------------------------------

    fn launch(cli: &Cli) -> Result<Value, Fail> {
        let wanted: Option<u16> = match cli.get("--port") {
            Some(p) => Some(p.parse().map_err(|_| usage_err(format!("--port wants a number, not `{p}`")))?),
            None => None,
        };
        let already = match wanted {
            Some(p) => listening(p).then_some(p),
            None => discover(),
        };
        if let Some(port) = already {
            return status_at(cli, port, "connected", None);
        }
        let wait: f64 = cli.get("--wait").map(|w| w.parse().unwrap_or(60.0)).unwrap_or(60.0);
        let started = Instant::now();
        open_app(cli.get("--app"), wanted)?;
        // Ready when it listens and its token file is there: the first request must carry the token.
        let deadline = started + Duration::from_secs_f64(wait.max(1.0));
        while Instant::now() < deadline {
            let port = wanted.filter(|p| listening(*p)).or_else(|| if wanted.is_none() { discover() } else { None });
            if let Some(port) = port
                && APP.token_path(port).is_some_and(|t| t.exists())
            {
                // The window may still be loading: give it a moment to answer its first request.
                let mut last = String::new();
                while Instant::now() < deadline {
                    match status_at(cli, port, "launched", Some(started)) {
                        Ok(v) => return Ok(v),
                        Err(Fail(_, m)) => last = m,
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                return Err(Fail(EXIT_NO_APP, format!("SolidCraft opened on port {port} but did not answer within {wait}s: {last}")));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(Fail(
            EXIT_NO_APP,
            format!(
                "SolidCraft did not open its control channel within {wait}s (is Preferences › Assistant › \"Let AI agents connect\" on? \
                 or pass --port to launch it with --control)"
            ),
        ))
    }

    /// Start the app detached. Without a port the app's own setting decides where it listens (27878 by
    /// default) and its discovery file says so; with one it is started with `--control PORT`.
    fn open_app(app: Option<&str>, port: Option<u16>) -> Result<(), Fail> {
        let env_app = std::env::var("SOLIDCRAFT_APP").ok();
        let app = app.map(str::to_string).or(env_app);
        let mut extra: Vec<String> = Vec::new();
        if let Some(p) = port {
            extra.extend(["--control".to_string(), p.to_string()]);
        }
        let mut cmd = if cfg!(target_os = "macos") {
            let mut c = std::process::Command::new("open");
            c.arg("-a").arg(app.unwrap_or_else(|| "SolidCraft".into()));
            if !extra.is_empty() {
                c.arg("--args").args(&extra);
            }
            c
        } else {
            let mut c = std::process::Command::new(app.unwrap_or_else(|| "solidcraft".into()));
            c.args(&extra);
            c
        };
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().map_err(|e| Fail(EXIT_NO_APP, format!("could not start SolidCraft: {e} (set SOLIDCRAFT_APP or --app)")))?;
        if cfg!(target_os = "macos") {
            // `open` returns once LaunchServices has the request; its failure means the app wasn't found.
            match child.wait() {
                Ok(s) if s.success() => {}
                Ok(s) => return Err(Fail(EXIT_NO_APP, format!("`open -a` failed ({s}): is SolidCraft installed? (set SOLIDCRAFT_APP or --app)"))),
                Err(e) => return Err(Fail(EXIT_NO_APP, format!("could not start SolidCraft: {e}"))),
            }
        }
        Ok(())
    }

    fn status(cli: &Cli) -> Result<Value, Fail> {
        let addr = app_addr(cli)?;
        let port = port_of(&addr).unwrap_or_default();
        status_at(cli, port, "connected", None)
    }

    /// `{status, port, version, documents}` from the app at `port`: proves the token and the channel work.
    fn status_at(cli: &Cli, port: u16, how: &str, started: Option<Instant>) -> Result<Value, Fail> {
        let addr = cli.get("--connect").map(str::to_string).unwrap_or_else(|| format!("127.0.0.1:{port}"));
        let mut r = Remote::new(&addr, cli.get("--token"));
        let version = unwrap_result(&solidcraft_mcp::call_tool(&mut r, "version", &json!({})))?;
        let docs = r.call("engine.execute", json!({"command": "document.list", "params": {}})).ok();
        let mut out = json!({"ok": true, "status": how, "address": addr, "version": version});
        if let Some(d) = docs {
            out["documents"] = d.get("documents").cloned().unwrap_or(d);
        }
        if let Some(t) = started {
            out["seconds"] = json!((t.elapsed().as_secs_f64() * 10.0).round() / 10.0);
        }
        Ok(out)
    }

    // ---- batch ----------------------------------------------------------------------------------

    fn batch(cli: &Cli, rest: &[String]) -> Result<Value, Fail> {
        let src = rest.first().ok_or_else(|| usage_err("batch: give a file of steps, or - for stdin"))?;
        let doc = read_json(src)?;
        let (steps, mut args) = match doc {
            Value::Array(steps) => (steps, Map::new()),
            Value::Object(mut o) => {
                let steps = match o.remove("commands") {
                    Some(Value::Array(s)) => s,
                    _ => return Err(usage_err("batch: the file must hold an array of steps or {\"commands\": [...]}")),
                };
                (steps, o)
            }
            _ => return Err(usage_err("batch: the file must hold an array of steps or {\"commands\": [...]}")),
        };
        if steps.is_empty() {
            return Err(usage_err("batch: no steps"));
        }
        // `echo` is ours: which steps' results the summary keeps. The app never sees it.
        let mut echo: Vec<usize> = Vec::new();
        let mut clean = Vec::with_capacity(steps.len());
        for (i, s) in steps.into_iter().enumerate() {
            let Value::Object(mut o) = s else { return Err(usage_err(format!("batch: step {i} is not an object {{command, params}}"))) };
            if o.remove("echo").and_then(|v| v.as_bool()).unwrap_or(false) {
                echo.push(i);
            }
            let command = o.get("command").and_then(Value::as_str).unwrap_or_default().to_string();
            if let Some(p) = o.remove("params") {
                o.insert("params".into(), absolutize_step_paths(&command, p));
            }
            clean.push(Value::Object(o));
        }
        let n = clean.len();
        args.insert("commands".into(), Value::Array(clean));
        args.entry("verbose").or_insert(json!(false));
        if let Some(e) = cli.get("--expect") {
            args.insert("expect".into(), read_json(e)?);
        }
        if cli.has("--no-rollback") {
            args.insert("rollback".into(), json!(false));
        }
        if cli.has("--timings") {
            args.insert("timings".into(), json!(true));
        }
        let mode = cli.get("--results").unwrap_or("summary");
        if !["summary", "errors", "all"].contains(&mode) {
            return Err(usage_err(format!("--results is summary, errors or all, not `{mode}`")));
        }
        match call(cli, "batch", Value::Object(args)) {
            Ok(full) => Ok(if mode == "all" { full } else { summarize(&full, n, &echo, mode) }),
            // A failed batch comes back as its JSON in the error text: summarize that too.
            Err(Fail(code, msg)) => match serde_json::from_str::<Value>(&msg) {
                Ok(full @ Value::Object(_)) if mode != "all" => Err(Fail(code, summarize(&full, n, &echo, mode).to_string())),
                _ => Err(Fail(code, msg)),
            },
        }
    }

    /// The parts of a batch reply an agent acts on, without every step's result.
    fn summarize(full: &Value, n: usize, echo: &[usize], mode: &str) -> Value {
        let mut s = Map::new();
        let failed = full.get("failed").filter(|f| !f.is_null());
        let expectation_ok = full.get("expectation").and_then(|e| e.get("ok")).and_then(Value::as_bool).unwrap_or(true);
        s.insert("ok".into(), json!(failed.is_none() && expectation_ok));
        s.insert("steps".into(), json!(n));
        for k in ["completed", "rolled_back", "undone", "not_run", "total_ms", "note"] {
            if let Some(v) = full.get(k).filter(|v| !v.is_null()) {
                s.insert(k.into(), v.clone());
            }
        }
        if let Some(f) = failed {
            s.insert("failed".into(), f.clone());
        }
        if let Some(e) = full.get("expectation") {
            let mut e2 = Map::new();
            for k in ["ok", "measured", "mismatch"] {
                if let Some(v) = e.get(k).filter(|v| !v.as_array().is_some_and(Vec::is_empty)) {
                    e2.insert(k.into(), v.clone());
                }
            }
            s.insert("expectation".into(), Value::Object(e2));
        }
        let results = full.get("results").and_then(Value::as_array);
        // Bodies the batch made, changed or removed, across all steps: cheap and usually what matters.
        let mut created: Vec<Value> = Vec::new();
        let mut removed: Vec<Value> = Vec::new();
        for r in results.into_iter().flatten() {
            for (k, acc) in [("created_bodies", &mut created), ("removed_bodies", &mut removed)] {
                for b in r.get(k).and_then(Value::as_array).into_iter().flatten() {
                    if !acc.contains(b) {
                        acc.push(b.clone());
                    }
                }
            }
        }
        // After a rollback nothing the steps made is still there.
        let rolled_back = full.get("rolled_back").and_then(Value::as_bool).unwrap_or(false);
        if !created.is_empty() && !rolled_back {
            s.insert("created_bodies".into(), Value::Array(created));
        }
        if !removed.is_empty() && !rolled_back {
            s.insert("removed_bodies".into(), Value::Array(removed));
        }
        // Features or steps that left a warning are worth seeing even in a summary.
        let warnings: Vec<Value> = results
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(i, r)| r.get("warning").filter(|w| !w.is_null()).map(|w| json!({"step": i, "warning": w})))
            .collect();
        if !warnings.is_empty() {
            s.insert("warnings".into(), Value::Array(warnings));
        }
        if !echo.is_empty() {
            let mut kept = Map::new();
            for &i in echo {
                if let Some(r) = results.and_then(|r| r.get(i)) {
                    kept.insert(i.to_string(), r.clone());
                }
            }
            if !kept.is_empty() {
                s.insert("echo".into(), Value::Object(kept));
            }
        }
        if mode == "errors"
            && let (Some(f), Some(results)) = (failed, results)
            && let Some(i) = f.get("index").and_then(Value::as_u64)
            && let Some(r) = results.get(i as usize)
        {
            s.insert("failed_result".into(), r.clone());
        }
        Value::Object(s)
    }

    // ---- screenshot -----------------------------------------------------------------------------

    fn screenshot(cli: &Cli, rest: &[String]) -> Result<Value, Fail> {
        let mut args = match rest.first() {
            Some(a) => read_json(a)?,
            None => json!({}),
        };
        let out = match cli.get("--out").or_else(|| args.get("path").and_then(Value::as_str)) {
            Some(p) => absolute(p),
            None => temp_png("shot"),
        };
        if let Some(o) = args.as_object_mut() {
            // A model render by default: a window capture without a size is the whole app window.
            o.entry("source").or_insert(json!("model"));
            o.insert("path".into(), json!(out.to_string_lossy()));
        }
        let mut v = call(cli, "screenshot", args)?;
        // The app wrote the PNG at `path`; the copy decoded from the reply is a duplicate.
        if let Some(dup) = v.get("image").and_then(Value::as_str).map(PathBuf::from)
            && dup != out
        {
            let _ = std::fs::remove_file(dup);
        }
        if !out.exists() {
            return Err(Fail(EXIT_TOOL, format!("the screenshot was not written to {}", out.display())));
        }
        if let Some(o) = v.as_object_mut() {
            o.remove("images");
            o.insert("image".into(), json!(out.to_string_lossy()));
        }
        Ok(v)
    }

    fn temp_png(stem: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("solidcraft-agent");
        let _ = std::fs::create_dir_all(&dir);
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        dir.join(format!("{stem}-{t}-{}.png", std::process::id()))
    }

    /// A picture from a tool reply (base64), written to a temp file; its path.
    fn save_image(b64: &str) -> Option<String> {
        let bytes = base64_decode(b64)?;
        let p = temp_png("image");
        std::fs::write(&p, bytes).ok()?;
        Some(p.to_string_lossy().to_string())
    }

    fn base64_decode(s: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(s.len() * 3 / 4);
        let (mut acc, mut bits) = (0u32, 0u32);
        for c in s.bytes() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' | b'-' => 62,
                b'/' | b'_' => 63,
                b'=' | b'\n' | b'\r' | b' ' => continue,
                _ => return None,
            };
            acc = (acc << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(((acc >> bits) & 0xff) as u8);
            }
        }
        Some(out)
    }

    // ---- tools listing --------------------------------------------------------------------------

    fn tool_names() -> Vec<String> {
        solidcraft_mcp::tool_definitions().iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string)).collect()
    }

    fn tool_list() -> Value {
        let tools: Vec<Value> = solidcraft_mcp::tool_definitions()
            .iter()
            .map(|t| {
                let desc = t.get("description").and_then(Value::as_str).unwrap_or_default();
                // The first sentence is the summary.
                let first = desc.split(". ").next().unwrap_or(desc);
                json!({"name": t.get("name"), "summary": first})
            })
            .collect();
        json!({"tools": tools, "also": ["launch", "status", "exec", "batch", "tools", "help"]})
    }

    fn tool_help(name: &str) -> Result<Value, Fail> {
        solidcraft_mcp::tool_definitions()
            .into_iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
            .map(|t| json!({"name": name, "description": t.get("description"), "arguments": t.get("inputSchema")}))
            .ok_or_else(|| usage_err(format!("no tool `{name}`; `solidcraft-agent tools` lists them")))
    }

    // ---- arguments and paths ----------------------------------------------------------------------

    /// Inline JSON, `@file`, or `-` for stdin.
    fn read_json(arg: &str) -> Result<Value, Fail> {
        let text = if arg == "-" {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map_err(|e| usage_err(format!("stdin: {e}")))?;
            s
        } else if let Some(path) = arg.strip_prefix('@') {
            std::fs::read_to_string(path).map_err(|e| usage_err(format!("{path}: {e}")))?
        } else if !arg.trim_start().starts_with(['{', '[']) && Path::new(arg).is_file() {
            // A bare file name works too (batch build.json).
            std::fs::read_to_string(arg).map_err(|e| usage_err(format!("{arg}: {e}")))?
        } else {
            arg.to_string()
        };
        serde_json::from_str(&text).map_err(|e| usage_err(format!("not JSON ({e}): {}", text.chars().take(120).collect::<String>())))
    }

    /// The app resolves file paths against its own folder, not this shell's: make them absolute.
    fn absolute(p: &str) -> PathBuf {
        let path = Path::new(p);
        let expanded = match p.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).unwrap_or_else(|| path.to_path_buf()),
            None => path.to_path_buf(),
        };
        if expanded.is_absolute() { expanded } else { std::env::current_dir().map(|d| d.join(&expanded)).unwrap_or(expanded) }
    }

    /// Tools whose top-level `path` is a file.
    const PATH_TOOLS: &[&str] = &["save", "export", "open", "screenshot", "batch", "compare_picture"];

    /// Engine commands (and tools usable as batch steps) whose `path` is a file; elsewhere `path` can
    /// be something else (a sketch curve a text follows).
    fn is_file_command(c: &str) -> bool {
        PATH_TOOLS.contains(&c) || c.starts_with("file.") || c.starts_with("doc.") || matches!(c, "ui.screenshot" | "ui.render" | "pointcloud.insert")
    }

    fn absolutize_tool_paths(tool: &str, mut args: Value) -> Value {
        if PATH_TOOLS.contains(&tool) {
            fix_path(&mut args);
        }
        if tool == "execute"
            && let Some(c) = args.get("command").and_then(Value::as_str).map(str::to_string)
            && let Some(p) = args.get_mut("params")
        {
            *p = absolutize_step_paths(&c, p.take());
        }
        args
    }

    fn absolutize_step_paths(command: &str, mut params: Value) -> Value {
        if is_file_command(command) {
            fix_path(&mut params);
        }
        params
    }

    fn fix_path(v: &mut Value) {
        if let Some(o) = v.as_object_mut()
            && let Some(Value::String(p)) = o.get("path")
        {
            let abs = absolute(p).to_string_lossy().to_string();
            o.insert("path".into(), Value::String(abs));
        }
    }
}
