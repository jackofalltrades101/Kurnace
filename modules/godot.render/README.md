# godot.render — Godot 4.7.2 (.NET / C#)

Renders a C# Godot project to a PNG frame sequence (spec §12). It deliberately
does **not** accept GDScript-only projects.

## Requirements

- **Godot 4.7.2 .NET/Mono** (the C# build). This machine has it at
  `~/Applications/Godot_mono_4.7.2.app`.
  Set `KURN_GODOT` to the binary if it lives elsewhere.
- **.NET SDK 8+** on `PATH` (`dotnet`).
- macOS: MovieWriter needs a rendering display driver. The module briefly
  opens a Godot window because Godot 4.7.2's `--headless` dummy renderer
  crashes inside MovieWriter.

The module checks `godot --version`, requires the string `4.7.2`, and requires
`mono`; anything else is rejected with a clear error.

## Use

```sh
kurn lib add ./my-godot-project --json
kurn run godot.render --source ast_… --frames 120 --fps 30 --width 1920 --height 1080
```

Inputs:
- `source` — tree-hashed `godot` library asset (must contain `project.godot`
  and a `.csproj`; GDScript-only projects are rejected).
- `scene` — optional scene path (default: project main scene).
- `frames`, `fps`, `width`, `height`.

Outputs one `frames` directory asset (`kind = seq`) containing
`frame00000000.png…` and a deterministic `kurn.json` with `fps_num/fps_den`,
`frames`, `width`, `height`, `engine: godot`, `language: csharp`. The library
records that rate and duration, and `render.revideo` plays the sequence on the
timeline.

## C# model

The fixture in `test/fixture` is a minimal C# project:

```
project.godot   # dotnet/project/assembly_name
KurnShot.csproj # Godot.NET.Sdk/4.7.2, net8.0
Scripts/Shot.cs # partial class Shot : Node3D
Scenes/Shot.tscn
```

The module copies the project to its temp work dir, writes a `NuGet.Config`
pointing at the Godot install's bundled `GodotSharp/Tools/nupkgs`, runs
`dotnet build`, then:

```
Godot --path project --resolution WxH --rendering-method gl_compatibility \
      --rendering-driver opengl3 --audio-driver Dummy \
      --write-movie frames/frame.png --fixed-fps 30 --quit-after N
```
