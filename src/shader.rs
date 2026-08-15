use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::assets::{Shader, ShaderType};
use crate::tool::find_tool;

const GLSLES_VERTEX_SETUP: &str = "#define LOWPREC lowp\n";
const GLSLES_FRAGMENT_SETUP: &str = "precision mediump float;\n#define LOWPREC lowp\n";
const GLSL_VERTEX_SETUP: &str = "#version 120\n#define LOWPREC \n";
const GLSL_FRAGMENT_SETUP: &str = "#version 120\n#define LOWPREC \n";
const GLSLES_DEFINE: &str = "#define _YY_GLSLES_ 1\n";
const GLSL_DEFINE: &str = "#define _YY_GLSL_ 1\n";

// These files are embedded in the executable, so generated projects do not
// need the GMS installation at runtime.
const VERTEX_PREAMBLE: &str = include_str!("shader_preambles/VShaderCommon.shader");
const FRAGMENT_PREAMBLE: &str = include_str!("shader_preambles/FShaderCommon.shader");
const HLSL9_VERTEX_PREAMBLE: &str = include_str!("shader_preambles/HLSL9_VShaderCommon.shader");
const HLSL9_PIXEL_PREAMBLE: &str = include_str!("shader_preambles/HLSL9_PShaderCommon.shader");

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct ShaderData {
    pub name: String,
    pub kind: i32,
    pub glsles_vertex: String,
    pub glsles_fragment: String,
    pub glsl_vertex: String,
    pub glsl_fragment: String,
    pub hlsl9_vertex: String,
    pub hlsl9_pixel: String,
    pub hlsl11_vertex: Vec<u8>,
    pub hlsl11_pixel: Vec<u8>,
    pub attributes: Vec<String>,
}

pub fn prepare_shaders(shaders: &[Shader]) -> Result<Vec<ShaderData>, ShaderError> {
    let hlsl_compiler = find_hlsl_compiler();
    let hlsl11_compiler = if shaders
        .iter()
        .any(|shader| shader.shader_type == ShaderType::Hlsl11)
    {
        Some(find_hlsl11_compiler().ok_or(ShaderError::MissingCompiler {
            compiler: "D3D11 shader parser",
            executable: "D3D11ShaderParser.exe",
        })?)
    } else {
        None
    };
    shaders
        .iter()
        .map(|shader| prepare_shader(shader, hlsl_compiler.as_ref(), hlsl11_compiler.as_ref()))
        .collect()
}

fn prepare_shader(
    shader: &Shader,
    hlsl_compiler: Option<&HlslCompiler>,
    hlsl11_compiler: Option<&Hlsl11Compiler>,
) -> Result<ShaderData, ShaderError> {
    let vertex = shader.vertex_source();
    let fragment = shader.fragment_source();
    let attributes = vertex_attributes(vertex);
    let mut result = ShaderData {
        name: shader.name.clone(),
        kind: 0x8000_0000_u32.wrapping_add(shader_kind(shader.shader_type)) as i32,
        glsles_vertex: String::new(),
        glsles_fragment: String::new(),
        glsl_vertex: String::new(),
        glsl_fragment: String::new(),
        hlsl9_vertex: String::new(),
        hlsl9_pixel: String::new(),
        hlsl11_vertex: Vec::new(),
        hlsl11_pixel: Vec::new(),
        attributes,
    };

    match shader.shader_type {
        ShaderType::GlslEs => {
            result.glsles_vertex =
                join_shader(&[GLSLES_VERTEX_SETUP, VERTEX_PREAMBLE, GLSLES_DEFINE, vertex]);
            result.glsles_fragment = join_shader(&[
                GLSLES_FRAGMENT_SETUP,
                FRAGMENT_PREAMBLE,
                GLSLES_DEFINE,
                fragment,
            ]);
            result.glsl_vertex =
                join_shader(&[GLSL_VERTEX_SETUP, VERTEX_PREAMBLE, GLSL_DEFINE, vertex]);
            result.glsl_fragment = join_shader(&[
                GLSL_FRAGMENT_SETUP,
                FRAGMENT_PREAMBLE,
                GLSL_DEFINE,
                fragment,
            ]);
            if let Some(compiler) = hlsl_compiler {
                let (vertex, pixel) = compiler.compile(shader)?;
                result.hlsl9_vertex = join_shader(&[HLSL9_VERTEX_PREAMBLE, &vertex]);
                result.hlsl9_pixel = join_shader(&[HLSL9_PIXEL_PREAMBLE, &pixel]);
            }
        }
        ShaderType::Glsl => {
            result.glsl_vertex =
                join_shader(&[GLSL_VERTEX_SETUP, VERTEX_PREAMBLE, GLSL_DEFINE, vertex]);
            result.glsl_fragment = join_shader(&[
                GLSL_FRAGMENT_SETUP,
                FRAGMENT_PREAMBLE,
                GLSL_DEFINE,
                fragment,
            ]);
        }
        ShaderType::Hlsl9 => {
            result.hlsl9_vertex = join_shader(&[HLSL9_VERTEX_PREAMBLE, vertex]);
            result.hlsl9_pixel = join_shader(&[HLSL9_PIXEL_PREAMBLE, fragment]);
        }
        ShaderType::Hlsl11 => {
            let compiler = hlsl11_compiler.ok_or(ShaderError::MissingCompiler {
                compiler: "D3D11 shader parser",
                executable: "D3D11ShaderParser.exe",
            })?;
            let (vertex, pixel) = compiler.compile(shader)?;
            result.hlsl11_vertex = vertex;
            result.hlsl11_pixel = pixel;
        }
        ShaderType::Pssl | ShaderType::Cg | ShaderType::CgPs3 => {
            // These backends use target-specific compiled byte blobs. Their
            // SHDR slots remain null until the corresponding backend exists.
        }
    }
    Ok(result)
}

const fn shader_kind(kind: ShaderType) -> u32 {
    match kind {
        ShaderType::GlslEs => 1,
        ShaderType::Glsl => 2,
        ShaderType::Hlsl9 => 3,
        ShaderType::Hlsl11 => 4,
        ShaderType::Pssl => 5,
        ShaderType::Cg => 6,
        ShaderType::CgPs3 => 7,
    }
}

fn join_shader(parts: &[&str]) -> String {
    let capacity = parts.iter().map(|part| part.len()).sum();
    let mut result = String::with_capacity(capacity);
    for part in parts {
        result.push_str(part);
    }
    result
}

fn vertex_attributes(source: &str) -> Vec<String> {
    source
        .split('\n')
        .filter_map(|line| {
            let declaration = line.strip_prefix("attribute")?;
            let declaration = declaration.split_once(';')?.0;
            declaration
                .split_ascii_whitespace()
                .last()
                .map(str::to_owned)
        })
        .collect()
}

#[derive(Debug)]
struct HlslCompiler {
    executable: PathBuf,
    preamble: PathBuf,
}

impl HlslCompiler {
    fn compile(&self, shader: &Shader) -> Result<(String, String), ShaderError> {
        let source = fs::canonicalize(&shader.source).map_err(|source| ShaderError::Io {
            path: shader.source.clone(),
            source,
        })?;
        let output = TempDirectory::create()?;
        let mut output_argument = output.path.as_os_str().to_owned();
        output_argument.push(std::path::MAIN_SEPARATOR_STR);
        let process = Command::new(&self.executable)
            .current_dir(self.executable.parent().unwrap_or_else(|| Path::new(".")))
            .arg("-shader")
            .arg(&source)
            .arg("-name")
            .arg(format!("Shader_{}", shader.name))
            .arg("-out")
            .arg(output_argument)
            .arg("-preamble")
            .arg(&self.preamble)
            .arg("-typedefine")
            .arg("#define _YY_HLSL9_ 1")
            .output()
            .map_err(|source| ShaderError::Io {
                path: self.executable.clone(),
                source,
            })?;
        if !process.status.success() {
            return Err(ShaderError::CompilerFailed {
                shader: shader.name.clone(),
                compiler: "HLSL9 compiler",
                status: process.status.code(),
                output: String::from_utf8_lossy(&process.stderr).trim().to_owned(),
            });
        }
        let vertex_path = output.path.join("vout.shader");
        let pixel_path = output.path.join("fout.shader");
        let vertex = fs::read_to_string(&vertex_path).map_err(|source| ShaderError::Io {
            path: vertex_path,
            source,
        })?;
        let pixel = fs::read_to_string(&pixel_path).map_err(|source| ShaderError::Io {
            path: pixel_path,
            source,
        })?;
        Ok((vertex, pixel))
    }
}

#[derive(Debug)]
struct Hlsl11Compiler {
    executable: PathBuf,
    preamble: PathBuf,
}

impl Hlsl11Compiler {
    fn compile(&self, shader: &Shader) -> Result<(Vec<u8>, Vec<u8>), ShaderError> {
        let source = fs::canonicalize(&shader.source).map_err(|source| ShaderError::Io {
            path: shader.source.clone(),
            source,
        })?;
        let output = TempDirectory::create()?;
        let vertex_path = output.path.join("vout.shdata");
        let pixel_path = output.path.join("fout.shdata");
        let process = Command::new(&self.executable)
            .current_dir(self.executable.parent().unwrap_or_else(|| Path::new(".")))
            .arg("-quiet")
            .arg("-combinedshader")
            .arg("-profilev")
            .arg("vs_auto")
            .arg("-profilep")
            .arg("ps_auto")
            .arg("-preamble")
            .arg(&self.preamble)
            .arg("-shader")
            .arg(&source)
            .arg("-outv")
            .arg(&vertex_path)
            .arg("-outp")
            .arg(&pixel_path)
            .arg("-name")
            .arg(format!("Shader_{}", shader.name))
            .output()
            .map_err(|source| ShaderError::Io {
                path: self.executable.clone(),
                source,
            })?;
        if !process.status.success() {
            let stderr = String::from_utf8_lossy(&process.stderr);
            let stdout = String::from_utf8_lossy(&process.stdout);
            let output = [stderr.trim(), stdout.trim()]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(ShaderError::CompilerFailed {
                shader: shader.name.clone(),
                compiler: "D3D11 shader parser",
                status: process.status.code(),
                output,
            });
        }
        let vertex = read_compiler_output(shader, "vertex", &vertex_path)?;
        let pixel = read_compiler_output(shader, "pixel", &pixel_path)?;
        Ok((vertex, pixel))
    }
}

fn read_compiler_output(
    shader: &Shader,
    stage: &'static str,
    path: &Path,
) -> Result<Vec<u8>, ShaderError> {
    let bytes = fs::read(path).map_err(|source| ShaderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if bytes.is_empty() {
        return Err(ShaderError::EmptyCompilerOutput {
            shader: shader.name.clone(),
            stage,
            path: path.to_path_buf(),
        });
    }
    Ok(bytes)
}

fn find_hlsl_compiler() -> Option<HlslCompiler> {
    find_tool(&["HLSLCompiler.exe"]).and_then(|path| compiler_at(&path))
}

fn find_hlsl11_compiler() -> Option<Hlsl11Compiler> {
    find_tool(&["D3D11ShaderParser.exe"]).and_then(|path| hlsl11_compiler_at(&path))
}

fn compiler_at(path: &Path) -> Option<HlslCompiler> {
    if !path.is_file() {
        return None;
    }
    let executable = fs::canonicalize(path).ok()?;
    let preamble = executable.parent()?.to_path_buf();
    Some(HlslCompiler {
        executable,
        preamble,
    })
}

fn hlsl11_compiler_at(path: &Path) -> Option<Hlsl11Compiler> {
    if !path.is_file() {
        return None;
    }
    let executable = fs::canonicalize(path).ok()?;
    let preamble = executable.parent()?.to_path_buf();
    Some(Hlsl11Compiler {
        executable,
        preamble,
    })
}

#[derive(Debug)]
struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn create() -> Result<Self, ShaderError> {
        let root = env::temp_dir();
        for _ in 0..100 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("gmx-rs-shader-{}-{sequence}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(ShaderError::Io { path, source }),
            }
        }
        Err(ShaderError::TempDirectoryExhausted { root })
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
pub enum ShaderError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    MissingCompiler {
        compiler: &'static str,
        executable: &'static str,
    },
    CompilerFailed {
        shader: String,
        compiler: &'static str,
        status: Option<i32>,
        output: String,
    },
    EmptyCompilerOutput {
        shader: String,
        stage: &'static str,
        path: PathBuf,
    },
    TempDirectoryExhausted {
        root: PathBuf,
    },
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::MissingCompiler {
                compiler,
                executable,
            } => write!(
                formatter,
                "{compiler} ({executable}) was not found next to gmx or on PATH"
            ),
            Self::CompilerFailed {
                shader,
                compiler,
                status,
                output,
            } => write!(
                formatter,
                "{compiler} failed for shader {shader:?} with status {status:?}: {output}"
            ),
            Self::EmptyCompilerOutput {
                shader,
                stage,
                path,
            } => write!(
                formatter,
                "shader {shader:?} produced an empty {stage} output at {}",
                path.display()
            ),
            Self::TempDirectoryExhausted { root } => write!(
                formatter,
                "could not create a unique shader directory under {}",
                root.display()
            ),
        }
    }
}

impl std::error::Error for ShaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_attributes_in_source_order() {
        let source = "attribute vec3 in_Position;\n// attribute vec3 ignored;\nattribute vec4 in_Colour; // comment\n  attribute vec2 indented;";
        assert_eq!(
            vertex_attributes(source),
            ["in_Position".to_owned(), "in_Colour".to_owned()]
        );
    }
}
