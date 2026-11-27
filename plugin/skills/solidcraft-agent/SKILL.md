---
name: solidcraft-agent
description: Build and edit 3D CAD models live in the SolidCraft app from the shell with the `solidcraft-agent` CLI, with one command for a quick tweak or a whole batch build per call, and compact JSON replies. Use whenever the user asks to design, model, change, measure, render or export a part (STL/3MF/STEP) in SolidCraft, or asks for a 3D-printable part and SolidCraft is their CAD tool.
---

# Driving SolidCraft with solidcraft-agent

`solidcraft-agent` drives the **running SolidCraft app**, so the user watches the model change in its window. Every call is a short-lived process: it connects to the app, runs one action, prints one line of compact JSON and exits. The app keeps the design, its undo history and the view between calls. Nothing else runs in the background.

## 0. Make sure it is installed and the app is up

```bash
command -v solidcraft-agent || curl -fsSL https://raw.githubusercontent.com/stevegg/solidcraft-agent/main/install.sh | bash
solidcraft-agent launch        # connects to the open app, or opens SolidCraft and waits until it answers
```

`launch` and `status` print `{ok, address, version, documents}`. Exit status: **0** ok, **1** the tool reported an error (read `error`), **2** bad usage, **3** no app is running (run `launch`).

If the install script is missing or fails, see the README at https://github.com/stevegg/solidcraft-agent. The tool needs SolidCraft installed (https://docs.solidcraft.io/).

## 1. Look before you build

```bash
solidcraft-agent inspect_design '{"measure":false,"sketches":false}'   # what is already open
solidcraft-agent new_design '{"name":"Bracket","new_tab":true}'        # a fresh tab; leaves the user's other designs alone
```

Always start a new part in a **new tab** (`new_tab: true`) unless the user asked you to change the open design.

For command shapes, sketch-plane axes, ids and known traps, read SolidCraft's own reference **once per session** (about 20 KB):

```bash
solidcraft-agent skill_reference '{"skill":"solidcraft-modeling"}'
solidcraft-agent list_commands '{"filter":"chamfer","fields":["id","params"]}'   # never unfiltered (over 100 KB)
solidcraft-agent describe_command '{"id":"solid.extrude"}'
```

## 2. Two ways to work (same design, mix freely)

**Whole builds: `batch`.** This is the default for anything more than one step. All steps go in one call, and the batch is all-or-nothing (a failing step rolls back the whole batch).

```bash
solidcraft-agent batch - --expect '{"volume_mm3":11659.3,"bodies":1}' <<'EOF'
[{"command":"solid.box","params":{"length":40,"width":30,"height":10,"name":"Block"}},
 {"command":"solid.fillet","params":{"edges":[[40,15,10]],"radius":3,"name":"Round"}},
 {"command":"solid.hole","params":{"position":[20,15,10],"diameter":6,"name":"Hole"},"echo":true}]
EOF
```

- A step is `{command, params}`: an engine command id, or one of the tools `measure`, `save`, `export`, `set_parameter`, `inspect_design` and so on.
- The file can also be `{"commands":[...], "expect":{...}, "coalesce_parameters":true}`. Pass a file path instead of `-` for a saved build.
- **The reply is a summary by default:** `{ok, steps, completed, created_bodies, expectation, failed?, rolled_back?, not_run?, warnings?}`. Mark a step `"echo": true` to get its full result as well (e.g. a sketch step whose ids you need next).
- `--results errors` adds the failed step's full result, and `--results all` returns everything. Use these only when debugging, since they cost far more tokens.
- **Always give `--expect`** with the volume, body count and/or bbox you computed. A mismatch makes the call fail (exit 1) and names it, so a cut that missed can't pass silently.
- Each step is its own undo step. To take a whole batch back, run `solidcraft-agent exec edit.checkpoint` before it and `solidcraft-agent exec edit.restore_checkpoint` after.

**Small tweaks: one command per call.**

```bash
solidcraft-agent exec solid.chamfer '{"edges":[[90,0,4]],"distance":3,"distance2":2,"name":"Bevel"}'
solidcraft-agent exec timeline.edit '{"feature":"Block","set":{"length":"50"}}' --expect '{"bodies":1}'
solidcraft-agent set_parameter '{"name":"L","value":"120 mm"}'
solidcraft-agent undo
```

**Any tool** the SolidCraft MCP server has is a subcommand: `solidcraft-agent <tool> '<json>'`. Run `solidcraft-agent tools` for the list and `solidcraft-agent help <tool>` for one tool's arguments. JSON arguments may be inline, `@file.json`, or `-` for stdin.

## 3. Verify every stage

- After every build, check volume, body count and bbox against numbers you worked out beforehand (`--expect`, or `solidcraft-agent measure '{"summary":true}'`). `ok: true` alone proves nothing. A cut that leaves the volume unchanged has missed; a join that adds a body did not touch.
- To probe the real surface (a wall thickness, a groove depth), shoot rays: `solidcraft-agent exec view.pick '{"rays":[{"origin":[x,y,10],"dir":[0,0,-1]}]}'` returns the hit point and normal.
- To look at the part: `solidcraft-agent screenshot '{"view":"top","width":900,"height":600,"grid":false,"axes":false}' --out /tmp/shot.png`, then read the PNG. Use `"camera":{"target":[x,y,z],"yaw":-0.7,"pitch":0.6,"distance":45}` for close-ups.
- Hole and boss positions come from `body_topology '{"body":"Body1","faces":"cylinders"}'`, not from the volume.

## 4. Finish

```bash
solidcraft-agent save '{"path":"~/Parts/bracket.sc"}'
solidcraft-agent export '{"path":"~/Parts/bracket.3mf","quality":"fine"}'   # .stl .3mf .step .obj ...
```

Relative and `~` paths are made absolute by the tool. The app resolves paths against its own folder, so never pass a bare relative path through a raw `ui.*` method yourself.

## Keeping replies small

- Prefer one `batch` over many `exec` calls, and leave `--results` at `summary`.
- Use `measure '{"summary":true}'`, `inspect_design '{"timeline":"summary","bodies":"names","sketches":false}'`, and `list_commands` with `filter` plus `fields`.
- For long repeated geometry (ticks, text, hole grids), use one feature plus `solid.pattern.rectangular` / `solid.pattern.circular` instead of dozens of features. Generate big batch files with a short script rather than typing them.
- `body_topology` is 25–45 KB, so call it only when you need edge or face points.

## Traps seen in practice

- **Sketch planes:** on `XZ`, sketch y is **−Z** (normal +Y); on `YZ`, sketch x is +Y and y is +Z (normal +X). `offset` moves the plane along its normal.
- **`solid.box`:** `center:[x,y,z]` places it by its centre. Without it the box starts at the origin corner. Pass `operation` (`new`/`join`/`cut`) explicitly.
- **Edge picks** (`fillet`, `chamfer`) snap to the nearest edge within about 5% of the part's size; otherwise the call is refused with the distance. Use an edge's midpoint.
- **`solid.chamfer`:** `distance` runs along the face that faces up most, and `distance2` along the other face. Probe with `view.pick` to confirm which way round it went.
- **`sketch.text`:** the built-in font has no bold, so `bold:true` needs an installed font (e.g. `"font":"Arial"`). `align` and `vertical_align` (`top`/`baseline`) place the text against `at`; `angle` is in degrees.
- **Cut extrude from a sketch on a top face:** pass `"direction":"negative"` to cut down into the part.
- **Long batches** (hundreds of pattern instances) can take minutes. The call waits for the result (up to an hour), so don't resend it.
- A batch refuses steps that open files or switch tabs (`doc.open`, `new_design` with `new_tab`, `document.activate`). Send those as their own calls.

## 3D-printing defaults (0.4 mm nozzle) unless the user says otherwise

- Engraved or embossed detail: lines and gaps at least 0.5 mm wide, depth 0.6 mm (3 layers at 0.2 mm); text cap height at least 4 mm in a bold face (strokes about 0.6 mm).
- Leave at least 1 mm of material under any engraving, and 1.2 mm or more for walls that take load.
- Say in your summary which numbers you assumed.
