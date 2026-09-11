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

`gmx` provides four commands:

```text
gmx build [OPTIONS]  # compile data.win and runtime files
gmx check [OPTIONS]  # check GML and VM bytecode generation
gmx run [OPTIONS]    # compile and start the game (Windows only)
gmx clean [OPTIONS]  # remove build output and caches
```

Run them from a directory containing exactly one `*.project.gmx`. The project
is detected automatically, the `Default` configuration is used, and build
artifacts are written to `./build`.

All four commands accept an explicit project manifest:

```text
--manifest-path <PATH>
```

`build`, `check`, and `run` accept a GameMaker configuration name:

```text
--config <NAME>
```

`build`, `run`, and `clean` can use a different build directory:

```text
--target-dir <DIRECTORY>
```

For example:

```text
gmx build --manifest-path C:\games\Game\Game.project.gmx --target-dir C:\games\Game\build --config Default
```

Use `gmx --help` for the command list, `gmx help <command>` for command-specific
options, and `gmx --version` for the compiler version.

## External tools

Audio conversion is built into `gmx`; WAV, MP3, and Ogg Vorbis sources are
decoded and emitted as the PCM WAV, MP3, or Ogg Vorbis stream required by the
project settings. Compatible Ogg Vorbis sources are kept byte-for-byte to avoid
lossy re-encoding. Ogg encoding uses statically linked libogg and libvorbis, so
projects containing sounds do not require an external converter or audio DLL.
Keep `VORBIS_LICENSES.txt` with binary distributions of `gmx`.

GLSL ES shaders targeting the Windows Runner require `HLSLCompiler.exe` and its
companion files to generate HLSL9. HLSL11 shaders additionally require
`D3D11ShaderParser.exe` and its companion files.

The supplied files are under `dependencies`. Alternatively, place the tools
and their companion files together in a directory on `PATH`.

## Cache

Build, audio, and texture caches are stored under
`<target-dir>/.gmx-cache/{build,audio,texture}` and are removed by `gmx clean`.
