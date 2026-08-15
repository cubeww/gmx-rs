use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use md5::{Digest, Md5};
use rayon::prelude::*;

use crate::config::Config;
use crate::project::{ProjectManifest, ResourceKind, ResourceRef};
use crate::resources::{
    OBJECT_UNDEFINED, ResourceError, load_background, load_extension, load_font, load_object,
    load_path, load_room, load_sound, load_sprite, load_timeline, object_reference_index,
    parse_bool,
};
use crate::settings::{CompileSettings, DEFAULT_TARGET_MASK, SettingsError};

pub use crate::resources::{
    Action, ActionArgument, Background, CollisionType, EXTENSION_FUNCTION_ID_START, Extension,
    ExtensionConfig, ExtensionConstant, ExtensionFile, ExtensionFramework, ExtensionFunction,
    ExtensionProxyFile, Font, FontRange, GameObject, GamePath, Glyph, KerningPair, ObjectEvent,
    PathPoint, PhysicsShapePoint, ROOM_INSTANCE_ID_START, ROOM_TILE_ID_START, Room, RoomBackground,
    RoomInstance, RoomMakerSettings, RoomTile, RoomView, Sound, Sprite, SpriteFrame, SpriteType,
    Timeline, TimelineEntry,
};

type ExtensionMemberIndex = HashMap<String, (usize, usize, usize)>;

pub const SHADER_MARKER: &str =
    "//######################_==_YOYO_SHADER_MARKER_==_######################@~";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub index: usize,
    pub manifest_index: usize,
    pub name: String,
    pub source: PathBuf,
    pub source_name: PathBuf,
    pub code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ShaderType {
    GlslEs,
    Glsl,
    Hlsl9,
    Hlsl11,
    Pssl,
    Cg,
    CgPs3,
}

impl ShaderType {
    fn from_manifest(value: &str) -> Self {
        match value {
            "GLSL" => Self::Glsl,
            "HLSL9" => Self::Hlsl9,
            "HLSL11" => Self::Hlsl11,
            "PSSL" => Self::Pssl,
            "CG" => Self::Cg,
            "CG_PS3" => Self::CgPs3,
            _ => Self::GlslEs,
        }
    }
}

impl fmt::Display for ShaderType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::GlslEs => "GLSLES",
            Self::Glsl => "GLSL",
            Self::Hlsl9 => "HLSL9",
            Self::Hlsl11 => "HLSL11",
            Self::Pssl => "PSSL",
            Self::Cg => "CG",
            Self::CgPs3 => "CG_PS3",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shader {
    pub index: usize,
    pub manifest_index: usize,
    pub name: String,
    pub source: PathBuf,
    pub shader_type: ShaderType,
    raw_source: String,
    vertex_range: Range<usize>,
    fragment_range: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct BinaryFile {
    pub source: PathBuf,
    pub data: Arc<[u8]>,
    digest: OnceLock<[u8; 16]>,
}

impl BinaryFile {
    pub fn digest(&self) -> [u8; 16] {
        *self.digest.get_or_init(|| Md5::digest(&self.data).into())
    }
}

impl Shader {
    pub fn raw_source(&self) -> &str {
        &self.raw_source
    }

    pub fn vertex_source(&self) -> &str {
        &self.raw_source[self.vertex_range.clone()]
    }

    pub fn fragment_source(&self) -> &str {
        &self.raw_source[self.fragment_range.clone()]
    }
}

#[derive(Debug)]
pub struct Assets {
    pub config_index: usize,
    pub extensions: Vec<Extension>,
    pub sounds: Vec<Sound>,
    pub sprites: Vec<Sprite>,
    pub backgrounds: Vec<Background>,
    pub paths: Vec<GamePath>,
    pub scripts: Vec<Script>,
    pub shaders: Vec<Shader>,
    pub fonts: Vec<Font>,
    pub timelines: Vec<Timeline>,
    pub objects: Vec<GameObject>,
    pub rooms: Vec<Room>,
    pub next_room_instance_id: i32,
    pub next_room_tile_id: i32,
    pub next_extension_function_id: i32,
    pub settings: CompileSettings,
    pub binary_files: Vec<BinaryFile>,
    extension_index: HashMap<String, usize>,
    extension_function_index: ExtensionMemberIndex,
    extension_constant_index: ExtensionMemberIndex,
    sound_index: HashMap<String, usize>,
    sprite_index: HashMap<String, usize>,
    background_index: HashMap<String, usize>,
    path_index: HashMap<String, usize>,
    script_index: HashMap<String, usize>,
    shader_index: HashMap<String, usize>,
    font_index: HashMap<String, usize>,
    timeline_index: HashMap<String, usize>,
    object_index: HashMap<String, usize>,
    room_index: HashMap<String, usize>,
    room_instance_index: HashMap<String, i32>,
    binary_file_index: HashMap<PathBuf, usize>,
}

impl Assets {
    pub fn load(project: &ProjectManifest, config: &Config) -> Result<Self, AssetError> {
        let config_index = project
            .resources_of(ResourceKind::Config)
            .position(|resource| resource.source == config.source)
            .unwrap_or(0);
        let use_new_audio = match config.option("option_use_new_audio") {
            Some(value) => parse_bool(value).ok_or_else(|| AssetError::InvalidConfigOption {
                path: config.source.clone(),
                name: "option_use_new_audio",
                value: value.to_owned(),
            })?,
            None => true,
        };

        let extension_refs: Vec<_> = project.resources_of(ResourceKind::Extension).collect();
        let sound_refs: Vec<_> = project.resources_of(ResourceKind::Sound).collect();
        let sprite_refs: Vec<_> = project.resources_of(ResourceKind::Sprite).collect();
        let background_refs: Vec<_> = project.resources_of(ResourceKind::Background).collect();
        let path_refs: Vec<_> = project.resources_of(ResourceKind::Path).collect();
        let script_refs: Vec<_> = project.resources_of(ResourceKind::Script).collect();
        let shader_refs: Vec<_> = project.resources_of(ResourceKind::Shader).collect();
        let font_refs: Vec<_> = project.resources_of(ResourceKind::Font).collect();
        let timeline_refs: Vec<_> = project.resources_of(ResourceKind::Timeline).collect();
        let object_refs: Vec<_> = project.resources_of(ResourceKind::Object).collect();
        let room_refs: Vec<_> = project.resources_of(ResourceKind::Room).collect();

        let (
            (script_groups, shader_drafts),
            (
                (sound_drafts, sprite_drafts),
                (
                    (background_drafts, path_drafts),
                    (
                        font_drafts,
                        (timeline_drafts, (object_drafts, (room_drafts, extension_drafts))),
                    ),
                ),
            ),
        ) = rayon::join(
            || {
                rayon::join(
                    || {
                        script_refs
                            .par_iter()
                            .map(load_script)
                            .collect::<Result<Vec<_>, _>>()
                    },
                    || {
                        shader_refs
                            .par_iter()
                            .map(load_shader)
                            .collect::<Result<Vec<_>, _>>()
                    },
                )
            },
            || {
                rayon::join(
                    || {
                        rayon::join(
                            || {
                                sound_refs
                                    .par_iter()
                                    .map(|resource| {
                                        load_sound(resource, config_index, use_new_audio)
                                    })
                                    .collect::<Result<Vec<_>, _>>()
                            },
                            || {
                                sprite_refs
                                    .par_iter()
                                    .map(|resource| load_sprite(resource))
                                    .collect::<Result<Vec<_>, _>>()
                            },
                        )
                    },
                    || {
                        rayon::join(
                            || {
                                rayon::join(
                                    || {
                                        background_refs
                                            .par_iter()
                                            .map(|resource| load_background(resource))
                                            .collect::<Result<Vec<_>, _>>()
                                    },
                                    || {
                                        path_refs
                                            .par_iter()
                                            .map(|resource| load_path(resource))
                                            .collect::<Result<Vec<_>, _>>()
                                    },
                                )
                            },
                            || {
                                rayon::join(
                                    || {
                                        font_refs
                                            .par_iter()
                                            .map(|resource| load_font(resource))
                                            .collect::<Result<Vec<_>, _>>()
                                    },
                                    || {
                                        rayon::join(
                                            || {
                                                timeline_refs
                                                    .par_iter()
                                                    .map(|resource| load_timeline(resource))
                                                    .collect::<Result<Vec<_>, _>>()
                                            },
                                            || {
                                                rayon::join(
                                                    || {
                                                        object_refs
                                                            .par_iter()
                                                            .map(|resource| load_object(resource))
                                                            .collect::<Result<Vec<_>, _>>()
                                                    },
                                                    || {
                                                        rayon::join(
                                                            || {
                                                                room_refs
                                                                    .par_iter()
                                                                    .map(|resource| {
                                                                        load_room(resource)
                                                                    })
                                                                    .collect::<Result<Vec<_>, _>>()
                                                            },
                                                            || {
                                                                extension_refs
                                                                    .par_iter()
                                                                    .map(|resource| {
                                                                        load_extension(resource)
                                                                    })
                                                                    .collect::<Result<Vec<_>, _>>()
                                                            },
                                                        )
                                                    },
                                                )
                                            },
                                        )
                                    },
                                )
                            },
                        )
                    },
                )
            },
        );

        let sounds = sound_drafts?;
        let sprites = sprite_drafts?;
        let backgrounds = background_drafts?;
        let paths = path_drafts?;
        let fonts = font_drafts?;
        let mut timelines = timeline_drafts?;
        let mut objects = object_drafts?;
        let mut rooms = room_drafts?;
        let mut extensions = extension_drafts?;

        let mut scripts = Vec::new();
        for group in script_groups? {
            for draft in group {
                scripts.push(Script {
                    index: scripts.len(),
                    manifest_index: draft.manifest_index,
                    name: draft.name,
                    source: draft.source,
                    source_name: draft.source_name,
                    code: draft.code,
                });
            }
        }

        let shaders = shader_drafts?
            .into_iter()
            .enumerate()
            .map(|(index, draft)| Shader {
                index,
                manifest_index: draft.manifest_index,
                name: draft.name,
                source: draft.source,
                shader_type: draft.shader_type,
                raw_source: draft.raw_source,
                vertex_range: draft.vertex_range,
                fragment_range: draft.fragment_range,
            })
            .collect::<Vec<_>>();

        let sound_index = make_index(
            sounds
                .iter()
                .map(|sound| (sound.name.as_str(), sound.source.as_path())),
            "sound",
        )?;
        let sprite_index = make_index(
            sprites
                .iter()
                .map(|sprite| (sprite.name.as_str(), sprite.source.as_path())),
            "sprite",
        )?;
        let background_index = make_index(
            backgrounds
                .iter()
                .map(|background| (background.name.as_str(), background.source.as_path())),
            "background",
        )?;
        let path_index = make_index(
            paths
                .iter()
                .map(|path| (path.name.as_str(), path.source.as_path())),
            "path",
        )?;
        let script_index = make_index(
            scripts
                .iter()
                .map(|script| (script.name.as_str(), script.source.as_path())),
            "script",
        )?;
        let shader_index = make_index(
            shaders
                .iter()
                .map(|shader| (shader.name.as_str(), shader.source.as_path())),
            "shader",
        )?;
        let font_index = make_index(
            fonts
                .iter()
                .map(|font| (font.name.as_str(), font.source.as_path())),
            "font",
        )?;
        let timeline_index = make_index(
            timelines
                .iter()
                .map(|timeline| (timeline.name.as_str(), timeline.source.as_path())),
            "timeline",
        )?;
        let object_index = make_index(
            objects
                .iter()
                .map(|object| (object.name.as_str(), object.source.as_path())),
            "object",
        )?;
        let room_index = make_index(
            rooms
                .iter()
                .map(|room| (room.name.as_str(), room.source.as_path())),
            "room",
        )?;
        let extension_index = make_index(
            extensions
                .iter()
                .map(|extension| (extension.name.as_str(), extension.source.as_path())),
            "extension",
        )?;
        resolve_object_references(
            &mut objects,
            &mut timelines,
            &sprites,
            &sprite_index,
            &object_index,
        )?;
        let (next_room_instance_id, next_room_tile_id, room_instance_index) =
            resolve_room_references(&mut rooms, &background_index, &object_index)?;
        let (next_extension_function_id, extension_function_index, extension_constant_index) =
            finalize_extensions(&mut extensions);
        mark_used_extensions(
            &mut extensions,
            config,
            &scripts,
            &timelines,
            &objects,
            &rooms,
        )?;
        let binary_paths = collect_binary_paths(
            project,
            &extensions,
            &sounds,
            &sprites,
            &backgrounds,
            &fonts,
            config,
        );
        let binary_files = binary_paths
            .par_iter()
            .map(|source| {
                fs::read(source)
                    .map(|data| BinaryFile {
                        source: source.clone(),
                        data: Arc::from(data),
                        digest: OnceLock::new(),
                    })
                    .map_err(|source_error| AssetError::Io {
                        path: source.clone(),
                        source: source_error,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let settings = CompileSettings::new(project, config, &sounds, &rooms, &extensions)?;
        let binary_file_index = binary_files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.source.clone(), index))
            .collect();

        Ok(Self {
            config_index,
            extensions,
            sounds,
            sprites,
            backgrounds,
            paths,
            scripts,
            shaders,
            fonts,
            timelines,
            objects,
            rooms,
            next_room_instance_id,
            next_room_tile_id,
            next_extension_function_id,
            settings,
            binary_files,
            extension_index,
            extension_function_index,
            extension_constant_index,
            sound_index,
            sprite_index,
            background_index,
            path_index,
            script_index,
            shader_index,
            font_index,
            timeline_index,
            object_index,
            room_index,
            room_instance_index,
            binary_file_index,
        })
    }

    pub fn sound(&self, name: &str) -> Option<&Sound> {
        self.sound_index.get(name).map(|index| &self.sounds[*index])
    }

    pub fn extension(&self, name: &str) -> Option<&Extension> {
        self.extension_index
            .get(name)
            .map(|index| &self.extensions[*index])
    }

    pub fn extension_function(&self, name: &str) -> Option<&ExtensionFunction> {
        self.extension_function_index
            .get(name)
            .map(|&(extension, file, function)| {
                &self.extensions[extension].files[file].functions[function]
            })
    }

    pub fn extension_constant(&self, name: &str) -> Option<&ExtensionConstant> {
        self.extension_constant_index
            .get(name)
            .map(|&(extension, file, constant)| {
                &self.extensions[extension].files[file].constants[constant]
            })
    }

    pub fn sprite(&self, name: &str) -> Option<&Sprite> {
        self.sprite_index
            .get(name)
            .map(|index| &self.sprites[*index])
    }

    pub fn background(&self, name: &str) -> Option<&Background> {
        self.background_index
            .get(name)
            .map(|index| &self.backgrounds[*index])
    }

    pub fn path(&self, name: &str) -> Option<&GamePath> {
        self.path_index.get(name).map(|index| &self.paths[*index])
    }

    pub fn script(&self, name: &str) -> Option<&Script> {
        self.script_index
            .get(name)
            .map(|index| &self.scripts[*index])
    }

    pub fn shader(&self, name: &str) -> Option<&Shader> {
        self.shader_index
            .get(name)
            .map(|index| &self.shaders[*index])
    }

    pub fn font(&self, name: &str) -> Option<&Font> {
        self.font_index.get(name).map(|index| &self.fonts[*index])
    }

    pub fn timeline(&self, name: &str) -> Option<&Timeline> {
        self.timeline_index
            .get(name)
            .map(|index| &self.timelines[*index])
    }

    pub fn object(&self, name: &str) -> Option<&GameObject> {
        self.object_index
            .get(name)
            .map(|index| &self.objects[*index])
    }

    pub fn room(&self, name: &str) -> Option<&Room> {
        self.room_index.get(name).map(|index| &self.rooms[*index])
    }

    pub fn room_instance_id(&self, name: &str) -> Option<i32> {
        self.room_instance_index.get(name).copied()
    }

    pub fn binary_file(&self, source: &Path) -> Option<&BinaryFile> {
        self.binary_file_index
            .get(source)
            .map(|index| &self.binary_files[*index])
    }

    pub fn binary(&self, source: &Path) -> Option<&[u8]> {
        self.binary_file(source).map(|file| file.data.as_ref())
    }
}

#[derive(Debug)]
pub enum AssetError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Encoding {
        path: PathBuf,
        message: String,
    },
    Settings(SettingsError),
    Resource(ResourceError),
    InvalidConfigOption {
        path: PathBuf,
        name: &'static str,
        value: String,
    },
    UnknownReference {
        owner_kind: &'static str,
        owner: String,
        field: &'static str,
        target: String,
    },
    DuplicateName {
        kind: &'static str,
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
}

impl fmt::Display for AssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot read asset {}: {source}", path.display())
            }
            Self::Encoding { path, message } => {
                write!(
                    formatter,
                    "cannot decode asset {}: {message}",
                    path.display()
                )
            }
            Self::Settings(source) => source.fmt(formatter),
            Self::Resource(source) => source.fmt(formatter),
            Self::InvalidConfigOption { path, name, value } => write!(
                formatter,
                "invalid {name} value {value:?} in config {}",
                path.display()
            ),
            Self::UnknownReference {
                owner_kind,
                owner,
                field,
                target,
            } => write!(
                formatter,
                "{owner_kind} {owner:?} references unknown {field} {target:?}"
            ),
            Self::DuplicateName {
                kind,
                name,
                first,
                second,
            } => write!(
                formatter,
                "duplicate {kind} name {name:?} in {} and {}",
                first.display(),
                second.display()
            ),
        }
    }
}

impl Error for AssetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Resource(source) => Some(source),
            Self::Settings(source) => Some(source),
            _ => None,
        }
    }
}

impl From<ResourceError> for AssetError {
    fn from(error: ResourceError) -> Self {
        Self::Resource(error)
    }
}

impl From<SettingsError> for AssetError {
    fn from(error: SettingsError) -> Self {
        Self::Settings(error)
    }
}

fn collect_binary_paths(
    project: &ProjectManifest,
    extensions: &[Extension],
    sounds: &[Sound],
    sprites: &[Sprite],
    backgrounds: &[Background],
    fonts: &[Font],
    config: &Config,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |path: &Path| {
        if seen.insert(path.to_path_buf()) {
            paths.push(path.to_path_buf());
        }
    };

    for data_file in &project.data_files {
        if data_file.exists && data_file.enabled_for(&config.name, DEFAULT_TARGET_MASK) {
            push(&data_file.source);
        }
    }
    for extension in extensions {
        if !extension.used_for(&config.name, DEFAULT_TARGET_MASK) {
            continue;
        }
        for file in &extension.files {
            if !file.enabled_for(&config.name, DEFAULT_TARGET_MASK) {
                continue;
            }
            if file.kind == 2 {
                push(&file.source);
            } else {
                for filename in file.filenames_for_target(DEFAULT_TARGET_MASK) {
                    push(&extension.folder.join(filename));
                }
            }
        }
    }
    for sound in sounds {
        push(&sound.audio_source);
    }
    for sprite in sprites {
        for frame in &sprite.frames {
            push(&frame.source);
        }
        if let Some(source) = &sprite.swf_source {
            push(source);
        }
        if let Some(source) = &sprite.spine_source {
            push(source);
        }
    }
    for background in backgrounds {
        // The official 1.4 compiler keeps image-less backgrounds and writes a
        // null texture-page reference for them.
        if background.image_source.is_file() {
            push(&background.image_source);
        }
    }
    for font in fonts {
        push(&font.image_source);
    }
    paths
}

fn mark_used_extensions(
    extensions: &mut [Extension],
    config: &Config,
    scripts: &[Script],
    timelines: &[Timeline],
    objects: &[GameObject],
    rooms: &[Room],
) -> Result<(), AssetError> {
    let mut identifiers = HashSet::<String>::new();
    for script in scripts {
        collect_gml_identifiers(&script.code, &mut identifiers);
    }
    for timeline in timelines {
        for action in timeline.entries.iter().flat_map(|entry| &entry.actions) {
            collect_action_identifiers(action, &mut identifiers);
        }
    }
    for object in objects {
        for action in object
            .events
            .iter()
            .flatten()
            .flat_map(|event| &event.actions)
        {
            collect_action_identifiers(action, &mut identifiers);
        }
    }
    for room in rooms {
        collect_gml_identifiers(&room.code, &mut identifiers);
        for instance in &room.instances {
            collect_gml_identifiers(&instance.code, &mut identifiers);
        }
    }
    for extension in extensions.iter() {
        if !extension.enabled_for(&config.name, DEFAULT_TARGET_MASK) {
            continue;
        }
        for file in &extension.files {
            if file.enabled_for(&config.name, DEFAULT_TARGET_MASK) {
                if !file.init.is_empty() {
                    identifiers.insert(file.init.clone());
                }
                if !file.finalizer.is_empty() {
                    identifiers.insert(file.finalizer.clone());
                }
            }
        }
    }

    // Extension GML can call another included function. Iterate until no newly
    // selected GML file contributes identifiers.
    let mut changed = true;
    while changed {
        changed = false;
        for extension in extensions.iter_mut() {
            if !extension.enabled_for(&config.name, DEFAULT_TARGET_MASK) {
                continue;
            }
            for file in &mut extension.files {
                if !file.enabled_for(&config.name, DEFAULT_TARGET_MASK)
                    || !file
                        .functions
                        .iter()
                        .any(|function| identifiers.contains(&function.name))
                {
                    continue;
                }
                if !file.used {
                    file.used = true;
                    extension.used = true;
                    changed = true;
                    if file.kind == 2 {
                        let bytes = fs::read(&file.source).map_err(|source| AssetError::Io {
                            path: file.source.clone(),
                            source,
                        })?;
                        let source = decode_text(&file.source, &bytes)?;
                        collect_gml_identifiers(&source, &mut identifiers);
                    }
                }
            }
        }
    }
    Ok(())
}

fn collect_action_identifiers(action: &Action, identifiers: &mut HashSet<String>) {
    if !action.function_name.is_empty() {
        identifiers.insert(action.function_name.clone());
    }
    collect_gml_identifiers(&action.code, identifiers);
    for argument in &action.arguments {
        collect_gml_identifiers(&argument.value, identifiers);
    }
}

fn collect_gml_identifiers(source: &str, identifiers: &mut HashSet<String>) {
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            quote @ (b'\'' | b'"') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                    } else if bytes[index] == quote {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                // Identifier bytes are restricted to ASCII by the conditions
                // above, so this conversion cannot fail.
                identifiers.insert(source[start..index].to_owned());
            }
            _ => index += 1,
        }
    }
}

fn finalize_extensions(
    extensions: &mut [Extension],
) -> (i32, ExtensionMemberIndex, ExtensionMemberIndex) {
    let mut next_function_id = EXTENSION_FUNCTION_ID_START;
    let mut function_index = HashMap::new();
    let mut constant_index = HashMap::new();

    for (extension_position, extension) in extensions.iter_mut().enumerate() {
        if extension.product_id.trim().is_empty() {
            extension.product_id = default_extension_product_id(extension_position);
        }
        for (file_position, file) in extension.files.iter_mut().enumerate() {
            for (function_position, function) in file.functions.iter_mut().enumerate() {
                function.id = next_function_id;
                next_function_id += 1;
                function_index.entry(function.name.clone()).or_insert((
                    extension_position,
                    file_position,
                    function_position,
                ));
            }
            for (constant_position, constant) in file.constants.iter().enumerate() {
                constant_index.entry(constant.name.clone()).or_insert((
                    extension_position,
                    file_position,
                    constant_position,
                ));
            }
        }
    }

    (next_function_id, function_index, constant_index)
}

fn default_extension_product_id(extension_position: usize) -> String {
    let source = format!("flynncom.yoyogames.LOCAL{extension_position}lives\r\n");
    let digest = Md5::digest(source.as_bytes());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = String::with_capacity(32);
    for byte in digest {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn resolve_room_references(
    rooms: &mut [Room],
    background_index: &HashMap<String, usize>,
    object_index: &HashMap<String, usize>,
) -> Result<(i32, i32, HashMap<String, i32>), AssetError> {
    let mut next_instance_id = ROOM_INSTANCE_ID_START;
    let mut next_tile_id = ROOM_TILE_ID_START;
    let mut instance_index = HashMap::new();

    for room in rooms {
        for background in &mut room.backgrounds {
            background.background_index = resolve_optional_index_reference(
                "room",
                &room.name,
                "background",
                &background.background_name,
                background_index,
            )?;
        }
        for view in &mut room.views {
            view.object_index = resolve_optional_index_reference(
                "room",
                &room.name,
                "view object",
                &view.object_name,
                object_index,
            )?;
        }
        for instance in &mut room.instances {
            instance.object_index = resolve_optional_index_reference(
                "room",
                &room.name,
                "instance object",
                &instance.object_name,
                object_index,
            )?;
            instance.id = next_instance_id;
            instance_index
                .entry(instance.name.clone())
                .or_insert(next_instance_id);
            next_instance_id += 1;
        }
        for tile in &mut room.tiles {
            tile.background_index = resolve_optional_index_reference(
                "room",
                &room.name,
                "tile background",
                &tile.background_name,
                background_index,
            )?;
            tile.id = next_tile_id;
            next_tile_id += 1;
        }
    }

    Ok((next_instance_id, next_tile_id, instance_index))
}

fn resolve_optional_index_reference(
    owner_kind: &'static str,
    owner: &str,
    field: &'static str,
    target: &str,
    index: &HashMap<String, usize>,
) -> Result<i32, AssetError> {
    if target.is_empty() || target == "<undefined>" {
        return Ok(-1);
    }
    index
        .get(target)
        .map(|value| *value as i32)
        .ok_or_else(|| AssetError::UnknownReference {
            owner_kind,
            owner: owner.to_owned(),
            field,
            target: target.to_owned(),
        })
}

fn resolve_object_references(
    objects: &mut [GameObject],
    timelines: &mut [Timeline],
    sprites: &[Sprite],
    sprite_index: &HashMap<String, usize>,
    object_index: &HashMap<String, usize>,
) -> Result<(), AssetError> {
    for object in objects {
        let owner = object.name.as_str();
        object.sprite_index =
            resolve_sprite_reference("object", owner, "sprite", &object.sprite_name, sprite_index)?;
        object.mask_index = resolve_sprite_reference(
            "object",
            owner,
            "mask sprite",
            &object.mask_name,
            sprite_index,
        )?;
        object.parent_index = resolve_object_reference(
            "object",
            owner,
            "parent object",
            &object.parent_name,
            object_index,
        )?;

        for group in &mut object.events {
            for event in group {
                if let Some(name) = &event.subtype_name {
                    event.subtype = resolve_object_reference(
                        "object",
                        owner,
                        "collision event object",
                        name,
                        object_index,
                    )?;
                }
                resolve_actions("object", owner, &mut event.actions, object_index)?;
            }
        }

        if object.sprite_index >= 0 && !object.physics_shape_points.is_empty() {
            let sprite = &sprites[object.sprite_index as usize];
            let x_origin = sprite.x_origin as f32;
            let y_origin = sprite.y_origin as f32;
            if object.physics_shape > 0 {
                for point in &mut object.physics_shape_points {
                    point.x -= x_origin;
                    point.y -= y_origin;
                }
            } else {
                object.physics_shape_points[0].x -= x_origin;
                object.physics_shape_points[0].y -= y_origin;
            }
        }
    }

    for timeline in timelines {
        for entry in &mut timeline.entries {
            resolve_actions("timeline", &timeline.name, &mut entry.actions, object_index)?;
        }
    }
    Ok(())
}

fn resolve_actions(
    owner_kind: &'static str,
    owner: &str,
    actions: &mut [Action],
    object_index: &HashMap<String, usize>,
) -> Result<(), AssetError> {
    for action in actions {
        action.who = resolve_object_reference(
            owner_kind,
            owner,
            "action target object",
            &action.who_name,
            object_index,
        )?;
    }
    Ok(())
}

fn resolve_sprite_reference(
    owner_kind: &'static str,
    owner: &str,
    field: &'static str,
    target: &str,
    index: &HashMap<String, usize>,
) -> Result<i32, AssetError> {
    if target.is_empty() || target == "<undefined>" {
        return Ok(-1);
    }
    index
        .get(target)
        .map(|value| *value as i32)
        .ok_or_else(|| AssetError::UnknownReference {
            owner_kind,
            owner: owner.to_owned(),
            field,
            target: target.to_owned(),
        })
}

fn resolve_object_reference(
    owner_kind: &'static str,
    owner: &str,
    field: &'static str,
    target: &str,
    index: &HashMap<String, usize>,
) -> Result<i32, AssetError> {
    if target.is_empty() {
        return Ok(OBJECT_UNDEFINED);
    }
    if let Some(value) = object_reference_index(target) {
        return Ok(value);
    }
    index
        .get(target)
        .map(|value| *value as i32)
        .ok_or_else(|| AssetError::UnknownReference {
            owner_kind,
            owner: owner.to_owned(),
            field,
            target: target.to_owned(),
        })
}

struct ScriptDraft {
    manifest_index: usize,
    name: String,
    source: PathBuf,
    source_name: PathBuf,
    code: String,
}

fn load_script(resource: &&ResourceRef) -> Result<Vec<ScriptDraft>, AssetError> {
    let code = read_text(&resource.source)?;
    let parts = script_parts(&resource.name, &code);
    Ok(parts
        .into_iter()
        .map(|part| ScriptDraft {
            manifest_index: resource.index,
            source_name: if part.split {
                PathBuf::from(format!("{}.gml", part.name))
            } else {
                resource.source.clone()
            },
            name: part.name,
            source: resource.source.clone(),
            code: part.code,
        })
        .collect())
}

pub(crate) struct ScriptPart {
    pub name: String,
    pub code: String,
    pub split: bool,
}

pub(crate) fn script_parts(default_name: &str, code: &str) -> Vec<ScriptPart> {
    let lines = text_lines(code);
    if !lines
        .first()
        .is_some_and(|line| line.starts_with("#define"))
    {
        return vec![ScriptPart {
            name: default_name.to_owned(),
            code: code.to_owned(),
            split: false,
        }];
    }

    split_definitions(&lines)
        .into_iter()
        .map(|(name, body)| ScriptPart {
            name,
            code: format!("\n{body}"),
            split: true,
        })
        .collect()
}

fn split_definitions(lines: &[&str]) -> Vec<(String, String)> {
    let mut parts = Vec::new();
    let mut name = String::new();
    let mut body = String::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("#define") {
            if !name.is_empty() && !body.is_empty() {
                parts.push((name.clone(), std::mem::take(&mut body)));
            }
            name = trimmed
                .split([' ', '\t'])
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            body.clear();
        } else {
            body.push_str(line);
            body.push_str("\r\n");
        }
    }

    if !name.is_empty() && !body.is_empty() {
        parts.push((name, body));
    } else if name.is_empty() && !body.is_empty() {
        parts.push(("main".to_owned(), body));
    } else if !name.is_empty() && body.is_empty() {
        parts.push((name, String::new()));
    }
    parts
}

fn text_lines(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' || bytes[index] == b'\n' {
            lines.push(&text[start..index]);
            if bytes[index] == b'\r' && bytes.get(index + 1).is_some_and(|byte| *byte == b'\n') {
                index += 1;
            }
            start = index + 1;
        }
        index += 1;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

struct ShaderDraft {
    manifest_index: usize,
    name: String,
    source: PathBuf,
    shader_type: ShaderType,
    raw_source: String,
    vertex_range: Range<usize>,
    fragment_range: Range<usize>,
}

fn load_shader(resource: &&ResourceRef) -> Result<ShaderDraft, AssetError> {
    let raw_source = read_text(&resource.source)?;
    let (vertex_range, fragment_range) = shader_ranges(&raw_source);
    Ok(ShaderDraft {
        manifest_index: resource.index,
        name: resource.name.clone(),
        source: resource.source.clone(),
        shader_type: ShaderType::from_manifest(resource.shader_type.as_deref().unwrap_or("GLSLES")),
        raw_source,
        vertex_range,
        fragment_range,
    })
}

fn shader_ranges(source: &str) -> (Range<usize>, Range<usize>) {
    if let Some(marker) = source.find(SHADER_MARKER) {
        (0..marker, marker + SHADER_MARKER.len()..source.len())
    } else {
        (0..source.len(), 0..source.len())
    }
}

fn read_text(path: &Path) -> Result<String, AssetError> {
    let bytes = fs::read(path).map_err(|source| AssetError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    decode_text(path, &bytes)
}

fn decode_text(path: &Path, bytes: &[u8]) -> Result<String, AssetError> {
    if let Some(bytes) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(bytes.to_vec()).map_err(|error| AssetError::Encoding {
            path: path.to_path_buf(),
            message: error.to_string(),
        });
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(path, bytes, u16::from_le_bytes);
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(path, bytes, u16::from_be_bytes);
    }
    String::from_utf8(bytes.to_vec()).map_err(|error| AssetError::Encoding {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn decode_utf16(
    path: &Path,
    bytes: &[u8],
    decode: fn([u8; 2]) -> u16,
) -> Result<String, AssetError> {
    let mut chunks = bytes.chunks_exact(2);
    let units = chunks
        .by_ref()
        .map(|chunk| decode([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    if !chunks.remainder().is_empty() {
        return Err(AssetError::Encoding {
            path: path.to_path_buf(),
            message: "UTF-16 input has an odd byte count".to_owned(),
        });
    }
    String::from_utf16(&units).map_err(|error| AssetError::Encoding {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn make_index<'a>(
    resources: impl Iterator<Item = (&'a str, &'a Path)>,
    kind: &'static str,
) -> Result<HashMap<String, usize>, AssetError> {
    let mut index = HashMap::new();
    let mut sources: Vec<PathBuf> = Vec::new();
    for (position, (name, source)) in resources.enumerate() {
        if let Some(previous) = index.insert(name.to_owned(), position) {
            return Err(AssetError::DuplicateName {
                kind,
                name: name.to_owned(),
                first: sources[previous].clone(),
                second: source.to_path_buf(),
            });
        }
        sources.push(source.to_path_buf());
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::config::Config;
    use crate::package::collect_project_files;
    use crate::project::{ProjectManifest, ResourceKind, ResourceRef};
    use crate::resources::{
        EXTENSION_FUNCTION_ID_START, ROOM_INSTANCE_ID_START, ROOM_TILE_ID_START,
        extension_from_node, room_from_node,
    };
    use crate::xml;

    use super::{
        Assets, SHADER_MARKER, collect_gml_identifiers, decode_text, default_extension_product_id,
        finalize_extensions, resolve_object_reference, resolve_room_references,
        resolve_sprite_reference, script_parts, shader_ranges, text_lines,
    };

    #[test]
    fn skips_missing_files_disabled_by_config_or_unused_extensions() {
        let root = temp_dir("disabled-files");
        fs::create_dir_all(root.join("Configs")).unwrap();
        fs::create_dir_all(root.join("extensions")).unwrap();
        fs::write(
            root.join("Edge.project.gmx"),
            r#"<assets>
                <Configs><Config>Configs\Default</Config></Configs>
                <NewExtensions><extension index="0">extensions\Unused</extension></NewExtensions>
                <datafiles><datafile><name>missing.bin</name><exists>-1</exists>
                    <ConfigOptions><Config name="Default"><CopyToMask>0</CopyToMask></Config></ConfigOptions>
                    <filename>missing.bin</filename></datafile></datafiles>
            </assets>"#,
        )
        .unwrap();
        fs::write(
            root.join("Configs/Default.config.gmx"),
            "<Config><Options/></Config>",
        )
        .unwrap();
        fs::write(
            root.join("extensions/Unused.extension.gmx"),
            r#"<extension><name>Unused</name><files><file>
                <filename>missing.dll</filename><kind>1</kind>
                <functions><function><name>unused_call</name></function></functions>
            </file></files></extension>"#,
        )
        .unwrap();

        let project = ProjectManifest::load(root.join("Edge.project.gmx")).unwrap();
        let config = Config::load_from_project(&project, "Default").unwrap();
        let assets = Assets::load(&project, &config).unwrap();

        assert!(assets.binary_files.is_empty());
        assert!(!assets.extensions[0].used);
        assert!(!assets.extensions[0].files[0].used);
        assert!(collect_project_files(&project, &assets).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preserves_regular_script_source() {
        let code = "return 1;\r\n";
        let parts = script_parts("normal", code);

        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "normal");
        assert_eq!(parts[0].code, code);
        assert!(!parts[0].split);
    }

    #[test]
    fn splits_define_scripts_like_the_official_loader() {
        let code = "#define first\nreturn 1;\n#define second\r\nreturn 2;";
        let parts = script_parts("ignored", code);

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "first");
        assert_eq!(parts[0].code, "\nreturn 1;\r\n");
        assert_eq!(parts[1].name, "second");
        assert_eq!(parts[1].code, "\nreturn 2;\r\n");
        assert!(parts.iter().all(|part| part.split));
    }

    #[test]
    fn leading_space_disables_define_file_mode() {
        let code = " #define not_split\nreturn 1;";
        let parts = script_parts("original", code);

        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "original");
        assert_eq!(parts[0].code, code);
    }

    #[test]
    fn supports_all_official_line_endings() {
        assert_eq!(text_lines("a\rb\nc\r\nd"), ["a", "b", "c", "d"]);
        assert_eq!(text_lines("\n"), [""]);
        assert!(text_lines("").is_empty());
    }

    #[test]
    fn splits_shader_without_copying_source() {
        let source = format!("vertex{SHADER_MARKER}fragment");
        let (vertex, fragment) = shader_ranges(&source);

        assert_eq!(&source[vertex], "vertex");
        assert_eq!(&source[fragment], "fragment");

        let (vertex, fragment) = shader_ranges("both");
        assert_eq!(&"both"[vertex], "both");
        assert_eq!(&"both"[fragment], "both");
    }

    #[test]
    fn decodes_common_bom_encodings() {
        assert_eq!(
            decode_text(Path::new("utf8.gml"), b"\xEF\xBB\xBFhello").unwrap(),
            "hello"
        );
        assert_eq!(
            decode_text(Path::new("utf16.gml"), &[0xFF, 0xFE, b'h', 0, b'i', 0]).unwrap(),
            "hi"
        );
    }

    #[test]
    fn scans_gml_identifiers_without_matching_comments_or_strings() {
        let mut identifiers = HashSet::new();
        collect_gml_identifiers(
            "real_call(argument); // fake_line()\n/* fake_block */ text = \"fake_string()\";",
            &mut identifiers,
        );

        assert!(identifiers.contains("real_call"));
        assert!(identifiers.contains("argument"));
        assert!(identifiers.contains("text"));
        assert!(!identifiers.contains("fake_line"));
        assert!(!identifiers.contains("fake_block"));
        assert!(!identifiers.contains("fake_string"));
    }

    #[test]
    fn resolves_official_object_reference_sentinels_and_ids() {
        let sprites = HashMap::from([("hero".to_owned(), 3)]);
        let objects = HashMap::from([("enemy".to_owned(), 7)]);

        assert_eq!(
            resolve_sprite_reference("object", "player", "sprite", "hero", &sprites).unwrap(),
            3
        );
        assert_eq!(
            resolve_sprite_reference("object", "player", "mask sprite", "<undefined>", &sprites,)
                .unwrap(),
            -1
        );
        assert_eq!(
            resolve_object_reference("object", "player", "parent", "self", &objects).unwrap(),
            -1
        );
        assert_eq!(
            resolve_object_reference("object", "player", "collision", "enemy", &objects).unwrap(),
            7
        );
        assert!(
            resolve_object_reference("object", "player", "collision", "missing", &objects).is_err()
        );
    }

    #[test]
    fn finalizes_extension_product_and_function_ids_in_manifest_order() {
        fn parse_extension(
            name: &str,
            product_id: &str,
            functions: &str,
        ) -> crate::resources::Extension {
            let xml = format!(
                "<extension><name>{name}</name><ProductID>{product_id}</ProductID><files><file><filename>{name}.dll</filename><functions>{functions}</functions><constants><constant><name>{name}_CONST</name><value>1</value></constant></constants></file></files></extension>"
            );
            let root = xml::parse(Path::new("test.extension.gmx"), Cursor::new(xml)).unwrap();
            extension_from_node(
                &ResourceRef {
                    kind: ResourceKind::Extension,
                    index: 0,
                    name: name.to_owned(),
                    listed_path: name.to_owned(),
                    relative_path: PathBuf::from("test.extension.gmx"),
                    source: PathBuf::from("extensions/test.extension.gmx"),
                    shader_type: None,
                },
                &root,
            )
            .unwrap()
        }

        let function = |name: &str| {
            format!(
                "<function><name>{name}</name><externalName>{name}</externalName><kind>12</kind><returnType>2</returnType><argCount>0</argCount><args/></function>"
            )
        };
        let mut extensions = vec![
            parse_extension(
                "First",
                "",
                &format!("{}{}", function("first_a"), function("shared")),
            ),
            parse_extension(
                "Second",
                "ABCDEF",
                &format!("{}{}", function("second_a"), function("shared")),
            ),
        ];

        let (next_id, functions, constants) = finalize_extensions(&mut extensions);

        assert_eq!(extensions[0].product_id, "6603CE2BED9BFA02D3F9269AEB4D5C5A");
        assert_eq!(extensions[0].product_id, default_extension_product_id(0));
        assert_eq!(extensions[1].product_id, "ABCDEF");
        assert_eq!(
            extensions[0].files[0].functions[0].id,
            EXTENSION_FUNCTION_ID_START
        );
        assert_eq!(extensions[1].files[0].functions[0].id, 3);
        assert_eq!(next_id, 5);
        assert_eq!(functions["shared"], (0, 0, 1));
        assert_eq!(constants["Second_CONST"], (1, 0, 0));
    }

    #[test]
    fn resolves_room_references_and_assigns_global_ids_in_room_order() {
        fn parse_room(name: &str, body: &str) -> crate::resources::Room {
            let root = xml::parse(
                Path::new("test.room.gmx"),
                Cursor::new(format!("<room>{body}</room>")),
            )
            .unwrap();
            room_from_node(
                &ResourceRef {
                    kind: ResourceKind::Room,
                    index: 0,
                    name: name.to_owned(),
                    listed_path: name.to_owned(),
                    relative_path: PathBuf::from("test.room.gmx"),
                    source: PathBuf::from("test.room.gmx"),
                    shader_type: None,
                },
                &root,
            )
            .unwrap()
        }

        let background = r#"<background visible="-1" foreground="0" name="sky" x="0" y="0" htiled="0" vtiled="0" hspeed="0" vspeed="0" stretch="0"/>"#;
        let view = r#"<view visible="-1" objName="&lt;undefined&gt;" xview="0" yview="0" wview="320" hview="240" xport="0" yport="0" wport="640" hport="480" hborder="32" vborder="32" hspeed="-1" vspeed="-1"/>"#;
        let first_instance = r#"<instance objName="player" x="1" y="2" name="inst_first" code="" scaleX="1" scaleY="1" colour="4294967295" rotation="0"/>"#;
        let second_instance = r#"<instance objName="player" x="3" y="4" name="inst_second" code="" scaleX="1" scaleY="1" colour="4294967295" rotation="0"/>"#;
        let tile = r#"<tile bgName="tiles" x="0" y="0" w="16" h="16" xo="0" yo="0" depth="100" scaleX="1" scaleY="1" colour="4294967295"/>"#;
        let mut rooms = vec![
            parse_room(
                "first",
                &format!(
                    "<backgrounds>{background}</backgrounds><views>{view}</views><instances>{first_instance}</instances><tiles>{tile}</tiles>"
                ),
            ),
            parse_room(
                "second",
                &format!("<instances>{second_instance}</instances>"),
            ),
        ];
        let backgrounds = HashMap::from([("sky".to_owned(), 2), ("tiles".to_owned(), 5)]);
        let objects = HashMap::from([("player".to_owned(), 7)]);

        let (next_instance, next_tile, instance_names) =
            resolve_room_references(&mut rooms, &backgrounds, &objects).unwrap();

        assert_eq!(rooms[0].backgrounds[0].background_index, 2);
        assert_eq!(rooms[0].views[0].object_index, -1);
        assert_eq!(rooms[0].instances[0].object_index, 7);
        assert_eq!(rooms[0].instances[0].id, ROOM_INSTANCE_ID_START);
        assert_eq!(rooms[1].instances[0].id, ROOM_INSTANCE_ID_START + 1);
        assert_eq!(rooms[0].tiles[0].background_index, 5);
        assert_eq!(rooms[0].tiles[0].id, ROOM_TILE_ID_START);
        assert_eq!(next_instance, ROOM_INSTANCE_ID_START + 2);
        assert_eq!(next_tile, ROOM_TILE_ID_START + 1);
        assert_eq!(instance_names["inst_second"], ROOM_INSTANCE_ID_START + 1);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("gmx-rs-{label}-{}-{nonce}", std::process::id()))
    }
}
