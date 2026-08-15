# Windows dependencies

Copy this directory's files next to `gmx.exe` after a release build.

- `Runner.exe` is copied into each game build as `<project name>.exe`.
- `HLSLCompiler.exe`, `libEGL.dll`, `libGLESv2.dll`,
  `VShaderCommon.shader`, and `FShaderCommon.shader` generate HLSL9 for GLSL ES
  shaders used by the Windows Runner.
- `ffmpeg.exe` converts project audio.
- `D3D11ShaderParser.exe`, `d3dcompiler_46.dll`, and the two
  `HLSL11_*ShaderCommon.shader` files compile explicit HLSL11 shaders.
- `d3dx9_43.dll` is the legacy Direct3D helper used by the Runner. Ship it next
  to the generated game executable when the target system does not provide the
  legacy DirectX runtime.

Core Windows DLLs and Microsoft Visual C++ runtimes are system prerequisites
and are not copied from the development machine into this repository.
