# solidcraft-agent

A command-line tool built for AI agents to drive the **running [SolidCraft](https://docs.solidcraft.io/) CAD app**, plus a skill that teaches the agent to use it, packaged for both **Claude Code** (plugin and marketplace) and **OpenCode** (skill catalog).

- **Live:** the agent builds in the open SolidCraft window, and you watch the model appear.
- **No background process:** each call connects to the app, runs one action, prints one line of compact JSON and exits. The app keeps the design and its undo history between calls.
- **Two modes on the same design:** one command for a quick tweak, or a whole build as one `batch` call that returns only a short summary. Building a 71-step ruler takes one call and returns a 227-byte reply.
- **Same behaviour as SolidCraft's MCP server:** it reuses SolidCraft's own tool code (`solidcraft-mcp` as a library). `batch` is all-or-nothing with rollback, `--expect` checks volume, body count and bounding box, and every MCP tool is a subcommand.

```bash
solidcraft-agent launch                                  # connect to the open app, or open it and wait
solidcraft-agent batch build.json --expect '{"volume_mm3":11659.3,"bodies":1}'
solidcraft-agent exec solid.fillet '{"edges":[[40,15,10]],"radius":3}'
solidcraft-agent measure '{"summary":true}'
solidcraft-agent screenshot '{"view":"top"}' --out shot.png
solidcraft-agent export '{"path":"part.3mf","quality":"fine"}'
```

## Install

### 1. SolidCraft

Install the SolidCraft app (https://docs.solidcraft.io/guide/getting-started.html). Its control channel must be on: Preferences › Assistant › "Let AI agents connect" (on by default), or start it with `--control PORT`.

### 2. The CLI

```bash
curl -fsSL https://raw.githubusercontent.com/stevegg/solidcraft-agent/main/install.sh | bash
```

The script downloads the prebuilt binary for your machine from the latest [release](https://github.com/stevegg/solidcraft-agent/releases), checks its SHA-256 and installs it to `~/.local/bin/solidcraft-agent`. Prebuilt binaries cover macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64).

On any other platform, or with `SOLIDCRAFT_AGENT_FROM_SOURCE=1`, it builds from source instead. That installs [rustup](https://rustup.rs) if needed, plus SolidCraft's pinned Rust 1.99.1. The first build compiles SolidCraft's engine and takes several minutes.

Other environment options: `SOLIDCRAFT_AGENT_VERSION=v0.1.0` installs a specific release, and `SOLIDCRAFT_AGENT_INSTALL_DIR=...` changes where the binary goes.

To build by hand:

```bash
rustup toolchain install 1.99.1 --profile minimal
cargo +1.99.1 install --locked --git https://github.com/stevegg/solidcraft-agent solidcraft-agent
```

### 3. The Claude Code plugin (the skill)

In Claude Code:

```
/plugin marketplace add stevegg/solidcraft-agent
/plugin install solidcraft-agent@solidcraft-agent
```

or from a terminal:

```bash
claude plugin marketplace add stevegg/solidcraft-agent
claude plugin install solidcraft-agent@solidcraft-agent
```

To let the agent run the CLI without asking each time, add this to the `permissions.allow` list in `~/.claude/settings.json`:

```json
"Bash(solidcraft-agent:*)"
```

### 4. OpenCode (the same skill)

OpenCode reads the same `SKILL.md`. Pick one of these:

**a) Remote skill catalog** (updates automatically). Add this to `~/.config/opencode/opencode.json`, or to a project's `opencode.json`:

```jsonc
{
  "skills": ["https://raw.githubusercontent.com/stevegg/solidcraft-agent/main/plugin/skills/"]
}
```

**b) Installed by the script.** This copies the skill to `~/.config/opencode/skills/solidcraft-agent/`. Run it again to update:

```bash
curl -fsSL https://raw.githubusercontent.com/stevegg/solidcraft-agent/main/install.sh | bash -s -- --opencode
# skill only (CLI already installed):  ... | bash -s -- --opencode --skill-only
```

OpenCode also picks the skill up from `~/.claude/skills/` and `~/.agents/skills/` if you keep it there.

To let OpenCode run the CLI and load the skill without asking each time, add these to the `permissions` list in `opencode.json`:

```jsonc
{
  "permissions": [
    { "action": "shell", "resource": "solidcraft-agent *", "effect": "allow" },
    { "action": "skill", "resource": "solidcraft-agent", "effect": "allow" }
  ]
}
```

Then ask for a part, e.g. *"Design a 40 × 30 × 10 mm block with a 6 mm hole in SolidCraft and export it as 3MF."*

## Usage

```
solidcraft-agent launch [--wait SECS] [--app PATH]   connect to the open app, or open it and wait until it answers
solidcraft-agent status                              is the app up: address, version, open designs
solidcraft-agent exec <command> [PARAMS] [--expect JSON]
solidcraft-agent batch <FILE|-> [--expect JSON] [--results summary|errors|all] [--no-rollback] [--timings]
solidcraft-agent screenshot [ARGS] [--out FILE.png]
solidcraft-agent <tool> [ARGS]                       any SolidCraft MCP tool: measure, inspect_design, body_topology,
                                                     undo, redo, save, export, open, new_design, set_parameter,
                                                     check_parameters, list_commands, describe_command, skill_reference...
solidcraft-agent tools | help <tool>
```

- `ARGS` / `PARAMS` are inline JSON, `@file.json`, or `-` for stdin. Relative and `~` file paths are made absolute, because the app resolves paths against its own folder.
- Options anywhere:
  - `--port PORT` / `--connect HOST:PORT` choose which app. By default it uses the most recently started one, from its discovery file.
  - `--token T` gives the control token. By default it reads the app's token file.
  - `--param-check off|warn|strict` checks calls against the commands' contracts.
  - `--selection` adds the user's current selection to the reply.
  - `--pretty` indents the JSON.
- Exit status: `0` ok, `1` the tool reported an error, `2` bad usage, `3` no app is running.

### Batch files

A batch file holds an array of steps, or `{"commands": [...], "expect": {...}, "coalesce_parameters": true}`. Each step is `{"command": <engine command id or tool name>, "params": {...}}`, plus an optional `"echo": true`:

```json
[{"command":"solid.box","params":{"length":40,"width":30,"height":10,"name":"Block"}},
 {"command":"solid.fillet","params":{"edges":[[40,15,10]],"radius":3,"name":"Round"}},
 {"command":"solid.hole","params":{"position":[20,15,10],"diameter":6,"name":"Hole"},"echo":true}]
```

By default the reply is a summary: `{ok, steps, completed, created_bodies, expectation, failed?, rolled_back?, not_run?, warnings?, echo?}`. Only steps marked `echo` include their full result. `--results errors` adds the failed step's result, and `--results all` returns everything.

## How it works

SolidCraft's desktop app has a loopback control channel: JSON lines over TCP, authenticated with a random token the app writes to a file only the user can read (see SolidCraft's `docs/control-protocol.md`). `solidcraft-agent` finds the app through its discovery file (`control/current.port` in SolidCraft's data folder), reads the token, and runs SolidCraft's MCP tool functions (`solidcraft_mcp::call_tool_with`) against that connection directly. No MCP server is involved. `launch` starts the app (`open -a SolidCraft` on macOS, `solidcraft` elsewhere) when nothing answers, and waits for its channel and token.

The SolidCraft crates are pinned to one commit in `Cargo.toml`. To follow a newer SolidCraft, change every `rev` there together, then run `cargo update` and rebuild.

## Releasing

1. Bump `version` in `Cargo.toml`, `plugin/.claude-plugin/plugin.json` and `.claude-plugin/marketplace.json`.
2. Commit, then tag and push:
   ```bash
   git tag v0.2.0 && git push origin main v0.2.0
   ```
3. The `release` workflow builds the four platform binaries and publishes them, with `.sha256` files, on the GitHub Release for that tag. `install.sh` always fetches the latest release.

## Licence

MIT OR Apache-2.0, like SolidCraft. This is an independent tool, not part of the SolidCraft project.
