# gmx-rs

`gmx-rs` is a high-performance Rust compiler for GameMaker: Studio 1.4.9999
GMX projects. It produces a Windows `data.win` together with the files needed
by the original Runner.

## Build

```powershell
cargo build --release
Copy-Item dependencies\* target\release\ -Exclude README.md
```

The repository's `dependencies` directory contains the Windows Runner and
external compiler tools. They must be copied next to `gmx.exe`. You can replace
`Runner.exe` with another compatible Runner; `gmx` copies it to the output
directory as `<project name>.exe`.

## Usage

Run these commands in a directory containing exactly one `*.project.gmx`:

```text
gmx build    # write build/data.win and runtime files
gmx run      # build and start the game (Windows only)
gmx clean    # remove build output and caches
```

An explicit project, output file, and configuration can also be supplied:

```text
gmx build <project.gmx> <data.win> [config]
```

Use `gmx help` to list the inspection, extraction, validation, and diff
commands.

## External tools

Projects containing sounds require `ffmpeg.exe`.

GLSL ES shaders targeting the Windows Runner require `HLSLCompiler.exe` and its
companion files to generate HLSL9. HLSL11 shaders additionally require
`D3D11ShaderParser.exe` and its companion files.

The supplied files are under `dependencies`. Alternatively, place the tools
and their companion files together in a directory on `PATH`.

## Cache

Build, audio, and texture caches are stored under
`build/.gmx-cache/{build,audio,texture}` and are removed by `gmx clean`.
