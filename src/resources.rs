use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::path::gmx_path;
use crate::project::ResourceRef;
use crate::xml::{self, Node};

#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    pub index: usize,
    pub name: String,
    pub source: PathBuf,
    pub audio_source: PathBuf,
    pub original_name: String,
    pub kind: i32,
    pub extension: String,
    pub effects: i32,
    pub volume: f64,
    pub pan: f64,
    pub bit_rate: i32,
    pub sample_rate: i32,
    pub stereo: bool,
    pub bit_depth: i32,
    pub preload: bool,
    pub compressed: bool,
    pub streamed: bool,
    pub uncompress_on_load: bool,
    pub new_audio: bool,
    pub group_index: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Background {
    pub index: usize,
    pub name: String,
    pub source: PathBuf,
    pub image_source: PathBuf,
    pub tileset: bool,
    pub tile_width: i32,
    pub tile_height: i32,
    pub tile_x_offset: i32,
    pub tile_y_offset: i32,
    pub tile_horizontal_separation: i32,
    pub tile_vertical_separation: i32,
    pub horizontal_tile: bool,
    pub vertical_tile: bool,
    pub texture_groups: Vec<i32>,
    pub for_3d: bool,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathPoint {
    pub x: f64,
    pub y: f64,
    pub speed: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GamePath {
    pub index: usize,
    pub name: String,
    pub source: PathBuf,
    pub kind: i32,
    pub closed: bool,
    pub precision: i32,
    pub back_room: i32,
    pub horizontal_snap: bool,
    pub vertical_snap: bool,
    pub points: Vec<PathPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpriteType {
    Bitmap,
    Swf,
    Spine,
    Other(i32),
}

impl SpriteType {
    pub const fn value(self) -> i32 {
        match self {
            Self::Bitmap => 0,
            Self::Swf => 1,
            Self::Spine => 2,
            Self::Other(value) => value,
        }
    }
}

impl fmt::Display for SpriteType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bitmap => formatter.write_str("bitmap"),
            Self::Swf => formatter.write_str("SWF"),
            Self::Spine => formatter.write_str("Spine"),
            Self::Other(value) => write!(formatter, "sprite type {value}"),
        }
    }
}

impl From<i32> for SpriteType {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Bitmap,
            1 => Self::Swf,
            2 => Self::Spine,
            value => Self::Other(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionType {
    AxisAlignedRectangle,
    Precise,
    RotatedRectangle,
}

impl CollisionType {
    fn from_kind(value: i32) -> Self {
        match value {
            0 | 2 | 3 => Self::Precise,
            5 => Self::RotatedRectangle,
            _ => Self::AxisAlignedRectangle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteFrame {
    pub index: usize,
    pub source: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sprite {
    pub index: usize,
    pub name: String,
    pub source: PathBuf,
    pub sprite_type: SpriteType,
    pub width: i32,
    pub height: i32,
    pub x_origin: i32,
    pub y_origin: i32,
    pub collision_kind: i32,
    pub collision_type: CollisionType,
    pub collision_tolerance: i32,
    pub separate_masks: bool,
    pub bounding_box_mode: i32,
    pub bounding_box_left: i32,
    pub bounding_box_right: i32,
    pub bounding_box_top: i32,
    pub bounding_box_bottom: i32,
    pub horizontal_tile: bool,
    pub vertical_tile: bool,
    pub texture_groups: Vec<i32>,
    pub for_3d: bool,
    pub frames: Vec<SpriteFrame>,
    pub swf_source: Option<PathBuf>,
    pub swf_precision: f32,
    pub spine_source: Option<PathBuf>,
    pub playback_speed: f32,
    pub playback_speed_type: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontRange {
    pub first: i32,
    pub last: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KerningPair {
    pub other: i32,
    pub amount: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glyph {
    pub character: i32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub shift: i32,
    pub offset: i32,
    pub kerning: Vec<KerningPair>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Font {
    pub index: usize,
    pub name: String,
    pub source: PathBuf,
    pub font_name: String,
    pub size: i32,
    pub bold: bool,
    pub render_high_quality: bool,
    pub italic: bool,
    pub charset: i32,
    pub anti_alias: i32,
    pub include_ttf: bool,
    pub ttf_name: String,
    pub texture_groups: Vec<i32>,
    pub ranges: Vec<FontRange>,
    pub glyphs: Vec<Glyph>,
    pub first: i32,
    pub last: i32,
    pub image_source: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionArgument {
    pub kind: i32,
    pub value_kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub library_id: i32,
    pub id: i32,
    pub kind: i32,
    pub use_relative: bool,
    pub is_question: bool,
    pub use_apply_to: bool,
    pub execution_type: i32,
    pub function_name: String,
    pub code: String,
    pub who_name: String,
    pub who: i32,
    pub relative: bool,
    pub is_not: bool,
    pub arguments: Vec<ActionArgument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEntry {
    pub step: i32,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline {
    pub index: usize,
    pub name: String,
    pub source: PathBuf,
    pub entries: Vec<TimelineEntry>,
}

pub const OBJECT_EVENT_TYPE_COUNT: usize = 13;
pub const OBJECT_UNDEFINED: i32 = -100;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicsShapePoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectEvent {
    pub event_type: usize,
    pub subtype: i32,
    pub subtype_name: Option<String>,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GameObject {
    pub index: usize,
    pub name: String,
    pub source: PathBuf,
    pub sprite_name: String,
    pub sprite_index: i32,
    pub solid: bool,
    pub visible: bool,
    pub depth: i32,
    pub persistent: bool,
    pub parent_name: String,
    pub parent_index: i32,
    pub mask_name: String,
    pub mask_index: i32,
    pub events: Vec<Vec<ObjectEvent>>,
    pub physics_object: bool,
    pub physics_sensor: bool,
    pub physics_shape: i32,
    pub physics_density: f32,
    pub physics_restitution: f32,
    pub physics_group: i32,
    pub physics_linear_damping: f32,
    pub physics_angular_damping: f32,
    pub physics_friction: f32,
    pub physics_awake: bool,
    pub physics_kinematic: bool,
    pub physics_shape_points: Vec<PhysicsShapePoint>,
}

pub const ROOM_INSTANCE_ID_START: i32 = 100_000;
pub const ROOM_TILE_ID_START: i32 = 10_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomMakerSettings {
    pub is_set: bool,
    pub width: i32,
    pub height: i32,
    pub show_grid: bool,
    pub show_objects: bool,
    pub show_tiles: bool,
    pub show_backgrounds: bool,
    pub show_foregrounds: bool,
    pub show_views: bool,
    pub delete_underlying_objects: bool,
    pub delete_underlying_tiles: bool,
    pub page: i32,
    pub x_offset: i32,
    pub y_offset: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomBackground {
    pub visible: bool,
    pub foreground: bool,
    pub background_name: String,
    pub background_index: i32,
    pub x: i32,
    pub y: i32,
    pub horizontal_tile: bool,
    pub vertical_tile: bool,
    pub horizontal_speed: i32,
    pub vertical_speed: i32,
    pub stretch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomView {
    pub visible: bool,
    pub object_name: String,
    pub object_index: i32,
    pub x_view: i32,
    pub y_view: i32,
    pub width_view: i32,
    pub height_view: i32,
    pub x_port: i32,
    pub y_port: i32,
    pub width_port: i32,
    pub height_port: i32,
    pub horizontal_border: i32,
    pub vertical_border: i32,
    pub horizontal_speed: i32,
    pub vertical_speed: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoomInstance {
    pub id: i32,
    pub name: String,
    pub object_name: String,
    pub object_index: i32,
    pub x: i32,
    pub y: i32,
    pub code: String,
    pub scale_x: f64,
    pub scale_y: f64,
    pub color: u32,
    pub rotation: f64,
    pub locked: bool,
}

impl RoomInstance {
    /// Converts the GMX ABGR value to the ARGB value written to a GMS 1.4 ROOM record.
    pub const fn wad_color(&self) -> u32 {
        (self.color & 0xff00_ff00)
            | ((self.color & 0x00ff_0000) >> 16)
            | ((self.color & 0x0000_00ff) << 16)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoomTile {
    pub id: i32,
    pub listed_id: i32,
    pub name: String,
    pub background_name: String,
    pub background_index: i32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub source_x: i32,
    pub source_y: i32,
    pub depth: i32,
    pub scale_x: f64,
    pub scale_y: f64,
    pub color: u32,
    pub blend: i32,
    pub alpha: f64,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Room {
    pub index: usize,
    pub name: String,
    pub source: PathBuf,
    pub caption: String,
    pub width: i32,
    pub height: i32,
    pub vertical_snap: i32,
    pub horizontal_snap: i32,
    pub isometric: bool,
    pub speed: i32,
    pub persistent: bool,
    pub color: i32,
    pub show_color: bool,
    pub code: String,
    pub enable_views: bool,
    pub clear_view_background: bool,
    pub clear_display_buffer: bool,
    pub maker_settings: Option<RoomMakerSettings>,
    pub backgrounds: Vec<RoomBackground>,
    pub views: Vec<RoomView>,
    pub instances: Vec<RoomInstance>,
    pub tiles: Vec<RoomTile>,
    pub physics_world: bool,
    pub physics_world_top: i32,
    pub physics_world_left: i32,
    pub physics_world_right: i32,
    pub physics_world_bottom: i32,
    pub physics_world_gravity_x: f32,
    pub physics_world_gravity_y: f32,
    pub physics_world_pixels_to_meters: f32,
}

pub const EXTENSION_FUNCTION_ID_START: i32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionConfig {
    pub name: String,
    pub copy_to_mask: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFramework {
    pub name: String,
    pub weak: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionProxyFile {
    pub name: String,
    pub target_mask: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFunction {
    pub id: i32,
    pub name: String,
    pub external_name: String,
    pub kind: i32,
    pub help: String,
    pub return_type: i32,
    pub argument_count: i32,
    pub arguments: Vec<i32>,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionConstant {
    pub name: String,
    pub value: String,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFile {
    pub filename: String,
    pub original_name: String,
    pub source: PathBuf,
    pub init: String,
    pub finalizer: String,
    pub kind: i32,
    pub uncompress: bool,
    pub configs: Vec<ExtensionConfig>,
    pub proxy_files: Vec<ExtensionProxyFile>,
    pub functions: Vec<ExtensionFunction>,
    pub constants: Vec<ExtensionConstant>,
    pub used: bool,
}

impl ExtensionFile {
    pub fn enabled_for(&self, config: &str, target_mask: i64) -> bool {
        enabled_for_config(&self.configs, config, target_mask)
    }

    pub fn used_for(&self, config: &str, target_mask: i64) -> bool {
        self.enabled_for(config, target_mask)
            && (self.used || !self.init.is_empty() || !self.finalizer.is_empty())
    }

    pub fn filename_for_target(&self, target_mask: i64) -> &str {
        self.proxy_files
            .iter()
            .rev()
            .find(|proxy| !proxy.name.is_empty() && (proxy.target_mask & target_mask) != 0)
            .map_or(self.filename.as_str(), |proxy| proxy.name.as_str())
    }

    pub fn source_for_target(&self, folder: &Path, target_mask: i64) -> PathBuf {
        folder.join(gmx_path(self.filename_for_target(target_mask)))
    }

    pub fn filenames_for_target(&self, target_mask: i64) -> Vec<PathBuf> {
        let selected = self
            .proxy_files
            .iter()
            .filter(|proxy| !proxy.name.is_empty() && (proxy.target_mask & target_mask) != 0)
            .map(|proxy| gmx_path(&proxy.name))
            .collect::<Vec<_>>();
        if selected.is_empty() {
            vec![gmx_path(&self.filename)]
        } else {
            selected
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub index: usize,
    pub name: String,
    pub source: PathBuf,
    pub folder: PathBuf,
    pub version: String,
    pub author: String,
    pub date: String,
    pub license: String,
    pub description: String,
    pub help_file: String,
    pub install_directory: String,
    pub class_name: String,
    pub android_class_name: String,
    pub source_directory: PathBuf,
    pub android_source_directory: String,
    pub mac_source_directory: String,
    pub mac_linker_flags: String,
    pub mac_compiler_flags: String,
    pub package_id: String,
    pub product_id: String,
    pub android_inject: String,
    pub android_manifest_inject: String,
    pub android_activity_inject: String,
    pub gradle_inject: String,
    pub ios_plist_inject: String,
    pub ios_system_frameworks: Vec<ExtensionFramework>,
    pub ios_third_party_frameworks: Vec<ExtensionFramework>,
    pub configs: Vec<ExtensionConfig>,
    pub android_permissions: Vec<String>,
    pub included_resources: Vec<String>,
    pub files: Vec<ExtensionFile>,
    pub used: bool,
}

impl Extension {
    pub fn enabled_for(&self, config: &str, target_mask: i64) -> bool {
        enabled_for_config(&self.configs, config, target_mask)
    }

    pub fn used_for(&self, config: &str, target_mask: i64) -> bool {
        self.enabled_for(config, target_mask)
            && (self.used
                || self
                    .files
                    .iter()
                    .any(|file| file.used_for(config, target_mask)))
    }
}

fn enabled_for_config(configs: &[ExtensionConfig], config: &str, target_mask: i64) -> bool {
    configs
        .iter()
        .find(|entry| entry.name == config)
        .is_none_or(|entry| (entry.copy_to_mask & target_mask) != 0)
}

#[derive(Debug)]
pub enum ResourceError {
    Xml {
        path: PathBuf,
        message: String,
    },
    InvalidRoot {
        path: PathBuf,
        expected: &'static str,
        actual: String,
    },
    MissingField {
        path: PathBuf,
        field: &'static str,
    },
    InvalidField {
        path: PathBuf,
        field: String,
        value: String,
    },
    InvalidTextureGroup {
        path: PathBuf,
        name: String,
    },
    InvalidPathPoint {
        path: PathBuf,
        value: String,
    },
}

impl fmt::Display for ResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xml { path, message } => {
                write!(
                    formatter,
                    "cannot parse resource {}: {message}",
                    path.display()
                )
            }
            Self::InvalidRoot {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "invalid resource root {actual:?} in {}; expected {expected}",
                path.display()
            ),
            Self::MissingField { path, field } => {
                write!(formatter, "missing {field} in resource {}", path.display())
            }
            Self::InvalidField { path, field, value } => write!(
                formatter,
                "invalid {field} value {value:?} in resource {}",
                path.display()
            ),
            Self::InvalidTextureGroup { path, name } => write!(
                formatter,
                "invalid texture group field {name:?} in resource {}",
                path.display()
            ),
            Self::InvalidPathPoint { path, value } => write!(
                formatter,
                "invalid path point {value:?} in resource {}; expected x,y,speed",
                path.display()
            ),
        }
    }
}

impl Error for ResourceError {}

pub(crate) fn load_extension(resource: &ResourceRef) -> Result<Extension, ResourceError> {
    let root = load_root(resource, "extension")?;
    extension_from_node(resource, &root)
}

pub(crate) fn load_sound(
    resource: &ResourceRef,
    config_index: usize,
    use_new_audio: bool,
) -> Result<Sound, ResourceError> {
    let root = load_root(resource, "sound")?;
    sound_from_node(resource, &root, config_index, use_new_audio)
}

pub(crate) fn load_background(resource: &ResourceRef) -> Result<Background, ResourceError> {
    let root = load_root(resource, "background")?;
    background_from_node(resource, &root)
}

pub(crate) fn load_path(resource: &ResourceRef) -> Result<GamePath, ResourceError> {
    let root = load_root(resource, "path")?;
    path_from_node(resource, &root)
}

pub(crate) fn load_sprite(resource: &ResourceRef) -> Result<Sprite, ResourceError> {
    let root = load_root(resource, "sprite")?;
    sprite_from_node(resource, &root)
}

pub(crate) fn load_font(resource: &ResourceRef) -> Result<Font, ResourceError> {
    let root = load_root(resource, "font")?;
    font_from_node(resource, &root)
}

pub(crate) fn load_timeline(resource: &ResourceRef) -> Result<Timeline, ResourceError> {
    let root = load_root(resource, "timeline")?;
    timeline_from_node(resource, &root)
}

pub(crate) fn load_object(resource: &ResourceRef) -> Result<GameObject, ResourceError> {
    let root = load_root(resource, "object")?;
    object_from_node(resource, &root)
}

pub(crate) fn load_room(resource: &ResourceRef) -> Result<Room, ResourceError> {
    let root = load_root(resource, "room")?;
    room_from_node(resource, &root)
}

pub(crate) fn parse_bool(value: &str) -> Option<bool> {
    if let Ok(number) = value.trim().parse::<i64>() {
        return Some(number != 0);
    }
    match value.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn load_root(resource: &ResourceRef, expected: &'static str) -> Result<Node, ResourceError> {
    let root = xml::load(&resource.source).map_err(|error| ResourceError::Xml {
        path: resource.source.clone(),
        message: error.to_string(),
    })?;
    if root.name != expected {
        return Err(ResourceError::InvalidRoot {
            path: resource.source.clone(),
            expected,
            actual: root.name,
        });
    }
    Ok(root)
}

pub(crate) fn extension_from_node(
    resource: &ResourceRef,
    root: &Node,
) -> Result<Extension, ResourceError> {
    let name = required_text(root, "name", &resource.source)?.to_owned();
    let directory = resource.source.parent().unwrap_or_else(|| Path::new(""));
    let folder = directory.join(gmx_path(&name));
    let source_directory = folder.join("iOSSource");
    let files = root
        .child("files")
        .into_iter()
        .flat_map(|files| files.children_named("file"))
        .map(|file| extension_file_from_node(file, &folder, &resource.source))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Extension {
        index: resource.index,
        name,
        source: resource.source.clone(),
        folder,
        version: raw_text(root, "version").unwrap_or_default().to_owned(),
        author: raw_text(root, "author").unwrap_or_default().to_owned(),
        date: raw_text(root, "date").unwrap_or_default().to_owned(),
        license: raw_text(root, "license").unwrap_or_default().to_owned(),
        description: raw_text(root, "description").unwrap_or_default().to_owned(),
        help_file: raw_text(root, "helpfile").unwrap_or_default().to_owned(),
        install_directory: raw_text(root, "installdir").unwrap_or_default().to_owned(),
        class_name: raw_text(root, "classname").unwrap_or_default().to_owned(),
        android_class_name: raw_text(root, "androidclassname")
            .unwrap_or_default()
            .to_owned(),
        source_directory,
        android_source_directory: raw_text(root, "androidsourcedir")
            .unwrap_or_default()
            .to_owned(),
        mac_source_directory: raw_text(root, "macsourcedir")
            .unwrap_or_default()
            .to_owned(),
        mac_linker_flags: raw_text(root, "maclinkerflags")
            .unwrap_or_default()
            .to_owned(),
        mac_compiler_flags: raw_text(root, "maccompilerflags")
            .unwrap_or_default()
            .to_owned(),
        package_id: raw_text(root, "packageID").unwrap_or_default().to_owned(),
        product_id: raw_text(root, "ProductID").unwrap_or_default().to_owned(),
        android_inject: raw_text(root, "androidinject")
            .unwrap_or_default()
            .to_owned(),
        android_manifest_inject: raw_text(root, "androidmanifestinject")
            .unwrap_or_default()
            .to_owned(),
        android_activity_inject: raw_text(root, "androidactivityinject")
            .unwrap_or_default()
            .to_owned(),
        gradle_inject: raw_text(root, "gradleinject")
            .unwrap_or_default()
            .to_owned(),
        ios_plist_inject: raw_text(root, "iosplistinject")
            .unwrap_or_default()
            .to_owned(),
        ios_system_frameworks: extension_frameworks(root.child("iosSystemFrameworks")),
        ios_third_party_frameworks: extension_frameworks(root.child("iosThirdPartyFrameworks")),
        configs: extension_configs(root.child("ConfigOptions"), &resource.source)?,
        android_permissions: extension_text_items(root.child("androidPermissions")),
        included_resources: extension_text_items(root.child("IncludedResources")),
        files,
        used: false,
    })
}

fn extension_file_from_node(
    root: &Node,
    folder: &Path,
    path: &Path,
) -> Result<ExtensionFile, ResourceError> {
    let filename = required_text(root, "filename", path)?.to_owned();
    let functions = root
        .child("functions")
        .into_iter()
        .flat_map(|functions| functions.children_named("function"))
        .map(|function| extension_function_from_node(function, path))
        .collect::<Result<Vec<_>, _>>()?;
    let constants = root
        .child("constants")
        .into_iter()
        .flat_map(|constants| constants.children_named("constant"))
        .map(|constant| extension_constant_from_node(constant, path))
        .collect::<Result<Vec<_>, _>>()?;
    let proxy_files = root
        .child("ProxyFiles")
        .into_iter()
        .flat_map(|proxies| proxies.children_named("ProxyFile"))
        .map(|proxy| {
            Ok(ExtensionProxyFile {
                name: raw_text(proxy, "Name").unwrap_or_default().to_owned(),
                target_mask: number(proxy, "TargetMask", 0, path)?,
            })
        })
        .collect::<Result<Vec<_>, ResourceError>>()?;

    Ok(ExtensionFile {
        source: folder.join(gmx_path(&filename)),
        filename,
        original_name: raw_text(root, "origname").unwrap_or_default().to_owned(),
        init: raw_text(root, "init").unwrap_or_default().to_owned(),
        finalizer: raw_text(root, "final").unwrap_or_default().to_owned(),
        kind: number(root, "kind", 0, path)?,
        uncompress: number::<i32>(root, "uncompress", 0, path)? == -1,
        configs: extension_configs(root.child("ConfigOptions"), path)?,
        proxy_files,
        functions,
        constants,
        used: false,
    })
}

fn extension_function_from_node(
    root: &Node,
    path: &Path,
) -> Result<ExtensionFunction, ResourceError> {
    let arguments = root
        .child("args")
        .into_iter()
        .flat_map(|args| args.children_named("arg"))
        .map(|argument| parse_number(&argument.text, "function.args.arg", path))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ExtensionFunction {
        id: 0,
        name: raw_text(root, "name").unwrap_or_default().to_owned(),
        external_name: raw_text(root, "externalName")
            .unwrap_or_default()
            .to_owned(),
        kind: number(root, "kind", 0, path)?,
        help: raw_text(root, "help").unwrap_or_default().to_owned(),
        return_type: number(root, "returnType", 0, path)?,
        argument_count: number(root, "argCount", 0, path)?,
        arguments,
        hidden: boolean(root, "hidden", false, path)?,
    })
}

fn extension_constant_from_node(
    root: &Node,
    path: &Path,
) -> Result<ExtensionConstant, ResourceError> {
    Ok(ExtensionConstant {
        name: raw_text(root, "name").unwrap_or_default().to_owned(),
        value: raw_text(root, "value").unwrap_or_default().to_owned(),
        hidden: boolean(root, "hidden", false, path)?,
    })
}

fn extension_configs(
    root: Option<&Node>,
    path: &Path,
) -> Result<Vec<ExtensionConfig>, ResourceError> {
    root.into_iter()
        .flat_map(|configs| configs.children_named("Config"))
        .map(|config| {
            Ok(ExtensionConfig {
                name: attribute_text(config, "name", "Config.name", path)?.to_owned(),
                copy_to_mask: required_number(config, "CopyToMask", path)?,
            })
        })
        .collect()
}

fn extension_frameworks(root: Option<&Node>) -> Vec<ExtensionFramework> {
    root.into_iter()
        .flat_map(|frameworks| &frameworks.children)
        .map(|framework| ExtensionFramework {
            name: framework.text.clone(),
            weak: framework
                .attribute("weak")
                .is_some_and(|value| value != "0"),
        })
        .collect()
}

fn extension_text_items(root: Option<&Node>) -> Vec<String> {
    root.into_iter()
        .flat_map(|items| &items.children)
        .map(|item| item.text.clone())
        .collect()
}

fn sound_from_node(
    resource: &ResourceRef,
    root: &Node,
    config_index: usize,
    use_new_audio: bool,
) -> Result<Sound, ResourceError> {
    let original_name = required_text(root, "origname", &resource.source)?.to_owned();
    let extension = text(root, "extension").unwrap_or_default().to_owned();
    let audio_source = resolve_audio_source(&resource.source, &original_name);

    let type_value = selected_number(root, "types", config_index, -1, &resource.source)?;
    let group_index = if use_new_audio {
        number(root, "audioGroup", 0, &resource.source)?
    } else {
        0
    };

    Ok(Sound {
        index: resource.index,
        name: resource.name.clone(),
        source: resource.source.clone(),
        audio_source,
        original_name,
        kind: number(root, "kind", 0, &resource.source)?,
        extension,
        effects: number(root, "effects", 0, &resource.source)?,
        volume: selected_number(root, "volume", config_index, 0.0, &resource.source)?,
        pan: number(root, "pan", 0.0, &resource.source)?,
        bit_rate: selected_number(root, "bitRates", config_index, 192, &resource.source)?,
        sample_rate: selected_number(root, "sampleRates", config_index, 44_100, &resource.source)?,
        stereo: type_value == 1,
        bit_depth: selected_number(root, "bitDepths", config_index, 8, &resource.source)?,
        preload: boolean(root, "preload", false, &resource.source)?,
        compressed: boolean(root, "compressed", false, &resource.source)?,
        streamed: boolean(root, "streamed", false, &resource.source)?,
        uncompress_on_load: boolean(root, "uncompressOnLoad", false, &resource.source)?,
        new_audio: use_new_audio,
        group_index,
    })
}

fn background_from_node(resource: &ResourceRef, root: &Node) -> Result<Background, ResourceError> {
    let directory = resource.source.parent().unwrap_or_else(|| Path::new(""));
    let image_source = text(root, "data")
        .map(|data| directory.join(gmx_path(data)))
        .unwrap_or_default();

    Ok(Background {
        index: resource.index,
        name: resource.name.clone(),
        source: resource.source.clone(),
        image_source,
        tileset: boolean(root, "istileset", false, &resource.source)?,
        tile_width: number(root, "tilewidth", 0, &resource.source)?,
        tile_height: number(root, "tileheight", 0, &resource.source)?,
        tile_x_offset: number(root, "tilexoff", 0, &resource.source)?,
        tile_y_offset: number(root, "tileyoff", 0, &resource.source)?,
        tile_horizontal_separation: number(root, "tilehsep", 0, &resource.source)?,
        tile_vertical_separation: number(root, "tilevsep", 0, &resource.source)?,
        horizontal_tile: boolean(root, "HTile", false, &resource.source)?,
        vertical_tile: boolean(root, "VTile", false, &resource.source)?,
        texture_groups: texture_groups(root, &resource.source)?,
        for_3d: boolean(root, "For3D", false, &resource.source)?,
        width: number(root, "width", 0, &resource.source)?,
        height: number(root, "height", 0, &resource.source)?,
    })
}

fn path_from_node(resource: &ResourceRef, root: &Node) -> Result<GamePath, ResourceError> {
    let points = root
        .child("points")
        .into_iter()
        .flat_map(|points| points.children_named("point"))
        .map(|point| parse_path_point(&point.text, &resource.source))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(GamePath {
        index: resource.index,
        name: resource.name.clone(),
        source: resource.source.clone(),
        kind: number(root, "kind", 0, &resource.source)?,
        closed: boolean(root, "closed", false, &resource.source)?,
        precision: number(root, "precision", 0, &resource.source)?,
        back_room: number(root, "backroom", 0, &resource.source)?,
        horizontal_snap: boolean(root, "hsnap", false, &resource.source)?,
        vertical_snap: boolean(root, "vsnap", false, &resource.source)?,
        points,
    })
}

fn sprite_from_node(resource: &ResourceRef, root: &Node) -> Result<Sprite, ResourceError> {
    let directory = resource.source.parent().unwrap_or_else(|| Path::new(""));
    let collision_kind = number(root, "colkind", 0, &resource.source)?;
    let frames = root
        .child("frames")
        .into_iter()
        .flat_map(|frames| frames.children_named("frame"))
        .enumerate()
        .map(|(index, frame)| {
            let value = frame.text.trim();
            if value.is_empty() {
                return Err(ResourceError::MissingField {
                    path: resource.source.clone(),
                    field: "frames.frame",
                });
            }
            Ok(SpriteFrame {
                index,
                source: directory.join(gmx_path(value)),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Sprite {
        index: resource.index,
        name: resource.name.clone(),
        source: resource.source.clone(),
        sprite_type: SpriteType::from(number(root, "type", 0, &resource.source)?),
        width: number(root, "width", 0, &resource.source)?,
        height: number(root, "height", 0, &resource.source)?,
        x_origin: number(root, "xorig", 0, &resource.source)?,
        y_origin: number(root, "yorigin", 0, &resource.source)?,
        collision_kind,
        collision_type: CollisionType::from_kind(collision_kind),
        collision_tolerance: number(root, "coltolerance", 0, &resource.source)?,
        separate_masks: boolean(root, "sepmasks", false, &resource.source)?,
        bounding_box_mode: number(root, "bboxmode", 0, &resource.source)?,
        bounding_box_left: number(root, "bbox_left", 0, &resource.source)?,
        bounding_box_right: number(root, "bbox_right", 0, &resource.source)?,
        bounding_box_top: number(root, "bbox_top", 0, &resource.source)?,
        bounding_box_bottom: number(root, "bbox_bottom", 0, &resource.source)?,
        horizontal_tile: boolean(root, "HTile", false, &resource.source)?,
        vertical_tile: boolean(root, "VTile", false, &resource.source)?,
        texture_groups: texture_groups(root, &resource.source)?,
        for_3d: boolean(root, "For3D", false, &resource.source)?,
        frames,
        swf_source: optional_file(root, "SWFfile", directory),
        swf_precision: number(root, "SWFprecision", 0.5, &resource.source)?,
        spine_source: optional_file(root, "SpineFile", directory),
        playback_speed: number(root, "playbackSpeed", 0.0, &resource.source)?,
        playback_speed_type: number(root, "playbackSpeedType", 1, &resource.source)?,
    })
}

fn font_from_node(resource: &ResourceRef, root: &Node) -> Result<Font, ResourceError> {
    required_text(root, "image", &resource.source)?;

    let ranges = root
        .child("ranges")
        .into_iter()
        .flat_map(|ranges| ranges.children.iter())
        .map(|range| {
            let (first, last) = parse_integer_pair(&range.text, &range.name, &resource.source)?;
            Ok(FontRange { first, last })
        })
        .collect::<Result<Vec<_>, ResourceError>>()?;

    let mut glyphs = root
        .child("glyphs")
        .into_iter()
        .flat_map(|glyphs| glyphs.children_named("glyph"))
        .map(|glyph| {
            Ok(Glyph {
                character: attribute_number(
                    glyph,
                    "character",
                    "glyph.character",
                    &resource.source,
                )?,
                x: attribute_number(glyph, "x", "glyph.x", &resource.source)?,
                y: attribute_number(glyph, "y", "glyph.y", &resource.source)?,
                width: attribute_number(glyph, "w", "glyph.w", &resource.source)?,
                height: attribute_number(glyph, "h", "glyph.h", &resource.source)?,
                shift: attribute_number(glyph, "shift", "glyph.shift", &resource.source)?,
                offset: attribute_number(glyph, "offset", "glyph.offset", &resource.source)?,
                kerning: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, ResourceError>>()?;

    if let Some(pairs) = root.child("kerningPairs") {
        for pair in pairs.children_named("pair") {
            let first =
                attribute_number(pair, "first", "kerningPairs.pair.first", &resource.source)?;
            let other =
                attribute_number(pair, "second", "kerningPairs.pair.second", &resource.source)?;
            let amount =
                attribute_number(pair, "amount", "kerningPairs.pair.amount", &resource.source)?;
            if let Some(glyph) = glyphs.iter_mut().find(|glyph| glyph.character == first) {
                glyph.kerning.push(KerningPair { other, amount });
            }
        }
    }
    glyphs.sort_unstable_by_key(|glyph| glyph.character);
    let first = glyphs.first().map_or(0, |glyph| glyph.character);
    let last = glyphs.last().map_or(0, |glyph| glyph.character);

    let mut image_source = resource.source.with_extension("");
    image_source.set_extension("png");

    Ok(Font {
        index: resource.index,
        name: resource.name.clone(),
        source: resource.source.clone(),
        font_name: text(root, "name").unwrap_or_default().to_owned(),
        size: number(root, "size", 0, &resource.source)?,
        bold: boolean(root, "bold", false, &resource.source)?,
        render_high_quality: boolean(root, "renderhq", false, &resource.source)?,
        italic: boolean(root, "italic", false, &resource.source)?,
        charset: number(root, "charset", 0, &resource.source)?,
        anti_alias: number(root, "aa", 0, &resource.source)?,
        include_ttf: boolean(root, "includeTTF", false, &resource.source)?,
        ttf_name: text(root, "TTFName").unwrap_or_default().to_owned(),
        texture_groups: indexed_groups(
            root,
            "texgroup",
            "texgroups",
            "texgroup",
            &resource.source,
        )?,
        ranges,
        glyphs,
        first,
        last,
        image_source,
    })
}

fn timeline_from_node(resource: &ResourceRef, root: &Node) -> Result<Timeline, ResourceError> {
    let mut entries = Vec::new();
    for entry in root.children_named("entry") {
        let event = entry
            .child("event")
            .ok_or_else(|| ResourceError::MissingField {
                path: resource.source.clone(),
                field: "entry.event",
            })?;
        let actions = actions_from_node(event, &resource.source)?;
        if actions.is_empty() {
            continue;
        }
        entries.push(TimelineEntry {
            step: required_number(entry, "step", &resource.source)?,
            actions,
        });
    }

    Ok(Timeline {
        index: resource.index,
        name: resource.name.clone(),
        source: resource.source.clone(),
        entries,
    })
}

fn object_from_node(resource: &ResourceRef, root: &Node) -> Result<GameObject, ResourceError> {
    let sprite_name = text(root, "spriteName").unwrap_or_default().to_owned();
    let parent_name = text(root, "parentName").unwrap_or_default().to_owned();
    let mask_name = text(root, "maskName").unwrap_or_default().to_owned();
    let events = object_events(root.child("events"), &resource.source)?;
    let physics_shape_points = root
        .child("PhysicsShapePoints")
        .into_iter()
        .flat_map(|points| points.children_named("point"))
        .map(|point| {
            let (x, y) =
                parse_float_pair(&point.text, "PhysicsShapePoints.point", &resource.source)?;
            Ok(PhysicsShapePoint { x, y })
        })
        .collect::<Result<Vec<_>, ResourceError>>()?;

    Ok(GameObject {
        index: resource.index,
        name: resource.name.clone(),
        source: resource.source.clone(),
        sprite_name,
        sprite_index: -1,
        solid: boolean(root, "solid", false, &resource.source)?,
        visible: boolean(root, "visible", false, &resource.source)?,
        depth: number(root, "depth", 0, &resource.source)?,
        persistent: boolean(root, "persistent", false, &resource.source)?,
        parent_index: object_reference_index(&parent_name).unwrap_or(OBJECT_UNDEFINED),
        parent_name,
        mask_name,
        mask_index: -1,
        events,
        physics_object: boolean(root, "PhysicsObject", false, &resource.source)?,
        physics_sensor: boolean(root, "PhysicsObjectSensor", false, &resource.source)?,
        physics_shape: number(root, "PhysicsObjectShape", 0, &resource.source)?,
        physics_density: number(root, "PhysicsObjectDensity", 0.0, &resource.source)?,
        physics_restitution: number(root, "PhysicsObjectRestitution", 0.0, &resource.source)?,
        physics_group: number(root, "PhysicsObjectGroup", 0, &resource.source)?,
        physics_linear_damping: number(root, "PhysicsObjectLinearDamping", 0.0, &resource.source)?,
        physics_angular_damping: number(
            root,
            "PhysicsObjectAngularDamping",
            0.0,
            &resource.source,
        )?,
        physics_friction: number(root, "PhysicsObjectFriction", 0.0, &resource.source)?,
        physics_awake: boolean(root, "PhysicsObjectAwake", false, &resource.source)?,
        physics_kinematic: boolean(root, "PhysicsObjectKinematic", false, &resource.source)?,
        physics_shape_points,
    })
}

pub(crate) fn room_from_node(resource: &ResourceRef, root: &Node) -> Result<Room, ResourceError> {
    let maker_settings = root
        .child("makerSettings")
        .map(|settings| room_maker_settings_from_node(settings, &resource.source))
        .transpose()?;
    let backgrounds = root
        .child("backgrounds")
        .into_iter()
        .flat_map(|backgrounds| backgrounds.children_named("background"))
        .map(|background| room_background_from_node(background, &resource.source))
        .collect::<Result<Vec<_>, _>>()?;
    let views = root
        .child("views")
        .into_iter()
        .flat_map(|views| views.children_named("view"))
        .map(|view| room_view_from_node(view, &resource.source))
        .collect::<Result<Vec<_>, _>>()?;
    let instances = root
        .child("instances")
        .into_iter()
        .flat_map(|instances| instances.children_named("instance"))
        .map(|instance| room_instance_from_node(instance, &resource.source))
        .collect::<Result<Vec<_>, _>>()?;
    let tiles = root
        .child("tiles")
        .into_iter()
        .flat_map(|tiles| tiles.children_named("tile"))
        .map(|tile| room_tile_from_node(tile, &resource.source))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Room {
        index: resource.index,
        name: resource.name.clone(),
        source: resource.source.clone(),
        caption: raw_text(root, "caption").unwrap_or_default().to_owned(),
        width: number(root, "width", 0, &resource.source)?,
        height: number(root, "height", 0, &resource.source)?,
        vertical_snap: number(root, "vsnap", 0, &resource.source)?,
        horizontal_snap: number(root, "hsnap", 0, &resource.source)?,
        isometric: boolean(root, "isometric", false, &resource.source)?,
        speed: number(root, "speed", 0, &resource.source)?,
        persistent: boolean(root, "persistent", false, &resource.source)?,
        color: number(root, "colour", 0, &resource.source)?,
        show_color: boolean(root, "showcolour", false, &resource.source)?,
        code: raw_text(root, "code").unwrap_or_default().to_owned(),
        enable_views: boolean(root, "enableViews", false, &resource.source)?,
        clear_view_background: boolean(root, "clearViewBackground", false, &resource.source)?,
        clear_display_buffer: boolean(root, "clearDisplayBuffer", false, &resource.source)?,
        maker_settings,
        backgrounds,
        views,
        instances,
        tiles,
        physics_world: boolean(root, "PhysicsWorld", false, &resource.source)?,
        physics_world_top: number(root, "PhysicsWorldTop", 0, &resource.source)?,
        physics_world_left: number(root, "PhysicsWorldLeft", 0, &resource.source)?,
        physics_world_right: number(root, "PhysicsWorldRight", 0, &resource.source)?,
        physics_world_bottom: number(root, "PhysicsWorldBottom", 0, &resource.source)?,
        physics_world_gravity_x: number(root, "PhysicsWorldGravityX", 0.0, &resource.source)?,
        physics_world_gravity_y: number(root, "PhysicsWorldGravityY", 0.0, &resource.source)?,
        physics_world_pixels_to_meters: number(
            root,
            "PhysicsWorldPixToMeters",
            0.0,
            &resource.source,
        )?,
    })
}

fn room_maker_settings_from_node(
    root: &Node,
    path: &Path,
) -> Result<RoomMakerSettings, ResourceError> {
    Ok(RoomMakerSettings {
        is_set: boolean(root, "isSet", false, path)?,
        width: number(root, "w", 0, path)?,
        height: number(root, "h", 0, path)?,
        show_grid: boolean(root, "showGrid", false, path)?,
        show_objects: boolean(root, "showObjects", false, path)?,
        show_tiles: boolean(root, "showTiles", false, path)?,
        show_backgrounds: boolean(root, "showBackgrounds", false, path)?,
        show_foregrounds: boolean(root, "showForegrounds", false, path)?,
        show_views: boolean(root, "showViews", false, path)?,
        delete_underlying_objects: boolean(root, "deleteUnderlyingObj", false, path)?,
        delete_underlying_tiles: boolean(root, "deleteUnderlyingTiles", false, path)?,
        page: number(root, "page", 0, path)?,
        x_offset: number(root, "xoffset", 0, path)?,
        y_offset: number(root, "yoffset", 0, path)?,
    })
}

fn room_background_from_node(root: &Node, path: &Path) -> Result<RoomBackground, ResourceError> {
    Ok(RoomBackground {
        visible: attribute_boolean(root, "visible", "background.visible", path)?,
        foreground: attribute_boolean(root, "foreground", "background.foreground", path)?,
        background_name: attribute_text(root, "name", "background.name", path)?.to_owned(),
        background_index: -1,
        x: attribute_number(root, "x", "background.x", path)?,
        y: attribute_number(root, "y", "background.y", path)?,
        horizontal_tile: attribute_boolean(root, "htiled", "background.htiled", path)?,
        vertical_tile: attribute_boolean(root, "vtiled", "background.vtiled", path)?,
        horizontal_speed: attribute_number(root, "hspeed", "background.hspeed", path)?,
        vertical_speed: attribute_number(root, "vspeed", "background.vspeed", path)?,
        stretch: attribute_boolean(root, "stretch", "background.stretch", path)?,
    })
}

fn room_view_from_node(root: &Node, path: &Path) -> Result<RoomView, ResourceError> {
    Ok(RoomView {
        visible: attribute_boolean(root, "visible", "view.visible", path)?,
        object_name: attribute_text(root, "objName", "view.objName", path)?.to_owned(),
        object_index: -1,
        x_view: attribute_number(root, "xview", "view.xview", path)?,
        y_view: attribute_number(root, "yview", "view.yview", path)?,
        width_view: attribute_number(root, "wview", "view.wview", path)?,
        height_view: attribute_number(root, "hview", "view.hview", path)?,
        x_port: attribute_number(root, "xport", "view.xport", path)?,
        y_port: attribute_number(root, "yport", "view.yport", path)?,
        width_port: attribute_number(root, "wport", "view.wport", path)?,
        height_port: attribute_number(root, "hport", "view.hport", path)?,
        horizontal_border: attribute_number(root, "hborder", "view.hborder", path)?,
        vertical_border: attribute_number(root, "vborder", "view.vborder", path)?,
        horizontal_speed: attribute_number(root, "hspeed", "view.hspeed", path)?,
        vertical_speed: attribute_number(root, "vspeed", "view.vspeed", path)?,
    })
}

fn room_instance_from_node(root: &Node, path: &Path) -> Result<RoomInstance, ResourceError> {
    Ok(RoomInstance {
        id: 0,
        name: optional_attribute_text(root, "name")
            .unwrap_or_default()
            .to_owned(),
        object_name: attribute_text(root, "objName", "instance.objName", path)?.to_owned(),
        object_index: -1,
        x: attribute_number(root, "x", "instance.x", path)?,
        y: attribute_number(root, "y", "instance.y", path)?,
        code: attribute_text(root, "code", "instance.code", path)?.to_owned(),
        scale_x: attribute_number(root, "scaleX", "instance.scaleX", path)?,
        scale_y: attribute_number(root, "scaleY", "instance.scaleY", path)?,
        color: attribute_color(root, "colour", "instance.colour", path)?,
        rotation: attribute_number(root, "rotation", "instance.rotation", path)?,
        locked: optional_attribute_boolean(root, "locked", false, "instance.locked", path)?,
    })
}

fn room_tile_from_node(root: &Node, path: &Path) -> Result<RoomTile, ResourceError> {
    let color = attribute_color(root, "colour", "tile.colour", path)?;
    let blend = (((color & 0x00ff_0000) >> 16)
        | (color & 0x0000_ff00)
        | ((color & 0x0000_00ff) << 16)) as i32;

    Ok(RoomTile {
        id: 0,
        listed_id: optional_attribute_number(root, "id", 0, "tile.id", path)?,
        name: optional_attribute_text(root, "name")
            .unwrap_or_default()
            .to_owned(),
        background_name: attribute_text(root, "bgName", "tile.bgName", path)?.to_owned(),
        background_index: -1,
        x: attribute_number(root, "x", "tile.x", path)?,
        y: attribute_number(root, "y", "tile.y", path)?,
        width: attribute_number(root, "w", "tile.w", path)?,
        height: attribute_number(root, "h", "tile.h", path)?,
        source_x: attribute_number(root, "xo", "tile.xo", path)?,
        source_y: attribute_number(root, "yo", "tile.yo", path)?,
        depth: attribute_number(root, "depth", "tile.depth", path)?,
        scale_x: attribute_number(root, "scaleX", "tile.scaleX", path)?,
        scale_y: attribute_number(root, "scaleY", "tile.scaleY", path)?,
        color,
        blend,
        alpha: (color >> 24) as f64 / 255.0,
        locked: optional_attribute_boolean(root, "locked", false, "tile.locked", path)?,
    })
}

fn object_events(root: Option<&Node>, path: &Path) -> Result<Vec<Vec<ObjectEvent>>, ResourceError> {
    let mut groups = vec![Vec::new(); OBJECT_EVENT_TYPE_COUNT];
    let Some(root) = root else {
        return Ok(groups);
    };

    for event in root.children_named("event") {
        let event_type: usize = attribute_number(event, "eventtype", "event.eventtype", path)?;
        if event_type >= OBJECT_EVENT_TYPE_COUNT {
            return Err(ResourceError::InvalidField {
                path: path.to_path_buf(),
                field: "event.eventtype".to_owned(),
                value: event_type.to_string(),
            });
        }

        let subtype_name = event
            .attribute("ename")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let subtype = match subtype_name.as_deref() {
            Some(name) => object_reference_index(name).unwrap_or(OBJECT_UNDEFINED),
            None => match event.attribute("enumb") {
                Some(value) => parse_number(value, "event.enumb", path)?,
                None => 0,
            },
        };
        groups[event_type].push(ObjectEvent {
            event_type,
            subtype,
            subtype_name,
            actions: actions_from_node(event, path)?,
        });
    }
    Ok(groups)
}

pub(crate) fn object_reference_index(name: &str) -> Option<i32> {
    match name {
        "<undefined>" => Some(OBJECT_UNDEFINED),
        "other" => Some(-2),
        "self" => Some(-1),
        _ => None,
    }
}

pub(crate) fn actions_from_node(root: &Node, path: &Path) -> Result<Vec<Action>, ResourceError> {
    root.children_named("action")
        .map(|action| action_from_node(action, path))
        .collect()
}

fn action_from_node(root: &Node, path: &Path) -> Result<Action, ResourceError> {
    let arguments = root
        .child("arguments")
        .into_iter()
        .flat_map(|arguments| arguments.children_named("argument"))
        .map(|argument| {
            let value = argument
                .children
                .iter()
                .find(|child| child.name != "kind")
                .ok_or_else(|| ResourceError::MissingField {
                    path: path.to_path_buf(),
                    field: "action.arguments.argument.value",
                })?;
            Ok(ActionArgument {
                kind: required_number(argument, "kind", path)?,
                value_kind: value.name.clone(),
                value: value.text.clone(),
            })
        })
        .collect::<Result<Vec<_>, ResourceError>>()?;

    let who_name = text(root, "whoName").unwrap_or_default().to_owned();
    Ok(Action {
        library_id: number(root, "libid", 0, path)?,
        id: number(root, "id", 0, path)?,
        kind: number(root, "kind", 0, path)?,
        use_relative: boolean(root, "userelative", false, path)?,
        is_question: boolean(root, "isquestion", false, path)?,
        use_apply_to: boolean(root, "useapplyto", false, path)?,
        execution_type: number(root, "exetype", 0, path)?,
        function_name: raw_text(root, "functionname")
            .unwrap_or_default()
            .to_owned(),
        code: raw_text(root, "codestring").unwrap_or_default().to_owned(),
        who: object_reference_index(&who_name).unwrap_or(OBJECT_UNDEFINED),
        who_name,
        relative: boolean(root, "relative", false, path)?,
        is_not: boolean(root, "isnot", false, path)?,
        arguments,
    })
}

fn texture_groups(root: &Node, path: &Path) -> Result<Vec<i32>, ResourceError> {
    indexed_groups(root, "TextureGroup", "TextureGroups", "TextureGroup", path)
}

fn indexed_groups(
    root: &Node,
    single_name: &str,
    collection_name: &str,
    item_prefix: &str,
    path: &Path,
) -> Result<Vec<i32>, ResourceError> {
    let mut groups = Vec::new();
    for node in &root.children {
        if node.name == single_name {
            groups.push(parse_number(&node.text, single_name, path)?);
        } else if node.name == collection_name {
            groups.extend(std::iter::repeat_n(0, node.children.len()));
            for group in &node.children {
                let index = group
                    .name
                    .strip_prefix(item_prefix)
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|index| *index < groups.len())
                    .ok_or_else(|| ResourceError::InvalidTextureGroup {
                        path: path.to_path_buf(),
                        name: group.name.clone(),
                    })?;
                groups[index] = parse_number(&group.text, &group.name, path)?;
            }
        }
    }
    Ok(groups)
}

fn parse_path_point(value: &str, path: &Path) -> Result<PathPoint, ResourceError> {
    let mut values = value.split(',').map(str::trim);
    let Some(x) = values.next() else {
        return invalid_path_point(value, path);
    };
    let Some(y) = values.next() else {
        return invalid_path_point(value, path);
    };
    let Some(speed) = values.next() else {
        return invalid_path_point(value, path);
    };
    if values.next().is_some() {
        return invalid_path_point(value, path);
    }

    let parse = |part: &str| {
        part.parse::<f64>()
            .map_err(|_| ResourceError::InvalidPathPoint {
                path: path.to_path_buf(),
                value: value.to_owned(),
            })
    };
    Ok(PathPoint {
        x: parse(x)?,
        y: parse(y)?,
        speed: parse(speed)?,
    })
}

fn invalid_path_point<T>(value: &str, path: &Path) -> Result<T, ResourceError> {
    Err(ResourceError::InvalidPathPoint {
        path: path.to_path_buf(),
        value: value.to_owned(),
    })
}

fn resolve_audio_source(source: &Path, original_name: &str) -> PathBuf {
    let directory = source.parent().unwrap_or_else(|| Path::new(""));
    let listed = gmx_path(original_name);
    let file_name = listed.file_name().unwrap_or_default();
    let primary = if listed.is_absolute() || listed.starts_with(directory) {
        listed
    } else {
        directory.join("audio").join(file_name)
    };
    if primary.is_file() {
        return primary;
    }

    let audio_directory = primary.parent().unwrap_or(directory);
    let mut base = audio_directory.join(source.file_stem().unwrap_or_default());
    let mut extensions = Vec::new();
    if let Some(extension) = primary.extension().and_then(|value| value.to_str()) {
        extensions.push(extension.to_owned());
    }
    for extension in ["wav", "mp3", "ogg"] {
        if !extensions.iter().any(|value| value == extension) {
            extensions.push(extension.to_owned());
        }
    }
    for extension in extensions {
        base.set_extension(extension);
        if base.is_file() {
            return base;
        }
    }
    primary
}

fn optional_file(root: &Node, field: &str, directory: &Path) -> Option<PathBuf> {
    text(root, field).map(|value| directory.join(gmx_path(value)))
}

fn parse_integer_pair(value: &str, field: &str, path: &Path) -> Result<(i32, i32), ResourceError> {
    let mut values = value.split(',').map(str::trim);
    let Some(first) = values.next() else {
        return invalid_pair(value, field, path);
    };
    let Some(second) = values.next() else {
        return invalid_pair(value, field, path);
    };
    if values.next().is_some() {
        return invalid_pair(value, field, path);
    }
    Ok((
        parse_number(first, field, path)?,
        parse_number(second, field, path)?,
    ))
}

fn parse_float_pair(value: &str, field: &str, path: &Path) -> Result<(f32, f32), ResourceError> {
    let mut values = value.split(',').map(str::trim);
    let Some(first) = values.next() else {
        return invalid_pair(value, field, path);
    };
    let Some(second) = values.next() else {
        return invalid_pair(value, field, path);
    };
    if values.next().is_some() {
        return invalid_pair(value, field, path);
    }
    Ok((
        parse_number(first, field, path)?,
        parse_number(second, field, path)?,
    ))
}

fn invalid_pair<T>(value: &str, field: &str, path: &Path) -> Result<T, ResourceError> {
    Err(ResourceError::InvalidField {
        path: path.to_path_buf(),
        field: field.to_owned(),
        value: value.to_owned(),
    })
}

fn attribute_number<T>(
    node: &Node,
    attribute: &str,
    field: &'static str,
    path: &Path,
) -> Result<T, ResourceError>
where
    T: FromStr,
{
    let value = node
        .attribute(attribute)
        .ok_or_else(|| ResourceError::MissingField {
            path: path.to_path_buf(),
            field,
        })?;
    parse_number(value, field, path)
}

fn optional_attribute_number<T>(
    node: &Node,
    attribute: &str,
    default: T,
    field: &'static str,
    path: &Path,
) -> Result<T, ResourceError>
where
    T: FromStr,
{
    match node.attribute(attribute) {
        Some(value) => parse_number(value, field, path),
        None => Ok(default),
    }
}

fn attribute_text<'a>(
    node: &'a Node,
    attribute: &str,
    field: &'static str,
    path: &Path,
) -> Result<&'a str, ResourceError> {
    node.attribute(attribute)
        .ok_or_else(|| ResourceError::MissingField {
            path: path.to_path_buf(),
            field,
        })
}

fn optional_attribute_text<'a>(node: &'a Node, attribute: &str) -> Option<&'a str> {
    node.attribute(attribute)
}

fn attribute_boolean(
    node: &Node,
    attribute: &str,
    field: &'static str,
    path: &Path,
) -> Result<bool, ResourceError> {
    let value = attribute_text(node, attribute, field, path)?;
    parse_bool(value).ok_or_else(|| ResourceError::InvalidField {
        path: path.to_path_buf(),
        field: field.to_owned(),
        value: value.to_owned(),
    })
}

fn optional_attribute_boolean(
    node: &Node,
    attribute: &str,
    default: bool,
    field: &'static str,
    path: &Path,
) -> Result<bool, ResourceError> {
    match node.attribute(attribute) {
        Some(value) => parse_bool(value).ok_or_else(|| ResourceError::InvalidField {
            path: path.to_path_buf(),
            field: field.to_owned(),
            value: value.to_owned(),
        }),
        None => Ok(default),
    }
}

fn attribute_color(
    node: &Node,
    attribute: &str,
    field: &'static str,
    path: &Path,
) -> Result<u32, ResourceError> {
    let value = attribute_text(node, attribute, field, path)?;
    parse_number::<i64>(value, field, path).map(|value| value as u32)
}

fn text<'a>(root: &'a Node, field: &str) -> Option<&'a str> {
    root.child(field)
        .map(|node| node.text.trim())
        .filter(|value| !value.is_empty())
}

fn raw_text<'a>(root: &'a Node, field: &str) -> Option<&'a str> {
    root.child(field).map(|node| node.text.as_str())
}

fn required_text<'a>(
    root: &'a Node,
    field: &'static str,
    path: &Path,
) -> Result<&'a str, ResourceError> {
    text(root, field).ok_or_else(|| ResourceError::MissingField {
        path: path.to_path_buf(),
        field,
    })
}

fn number<T>(root: &Node, field: &str, default: T, path: &Path) -> Result<T, ResourceError>
where
    T: FromStr,
{
    match text(root, field) {
        Some(value) => parse_number(value, field, path),
        None => Ok(default),
    }
}

fn required_number<T>(root: &Node, field: &'static str, path: &Path) -> Result<T, ResourceError>
where
    T: FromStr,
{
    parse_number(required_text(root, field, path)?, field, path)
}

fn selected_number<T>(
    root: &Node,
    field: &str,
    config_index: usize,
    default: T,
    path: &Path,
) -> Result<T, ResourceError>
where
    T: FromStr,
{
    let Some(node) = root.child(field) else {
        return Ok(default);
    };
    let value = node
        .children
        .get(config_index)
        .map(|child| child.text.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| node.text.trim());
    if value.is_empty() {
        Ok(default)
    } else {
        parse_number(value, field, path)
    }
}

fn parse_number<T>(value: &str, field: &str, path: &Path) -> Result<T, ResourceError>
where
    T: FromStr,
{
    value
        .trim()
        .parse()
        .map_err(|_| ResourceError::InvalidField {
            path: path.to_path_buf(),
            field: field.to_owned(),
            value: value.to_owned(),
        })
}

fn boolean(root: &Node, field: &str, default: bool, path: &Path) -> Result<bool, ResourceError> {
    match text(root, field) {
        Some(value) => parse_bool(value).ok_or_else(|| ResourceError::InvalidField {
            path: path.to_path_buf(),
            field: field.to_owned(),
            value: value.to_owned(),
        }),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::{Path, PathBuf};

    use crate::project::{ResourceKind, ResourceRef};
    use crate::xml;

    use super::{
        CollisionType, OBJECT_EVENT_TYPE_COUNT, OBJECT_UNDEFINED, SpriteType, background_from_node,
        extension_from_node, font_from_node, object_from_node, path_from_node, room_from_node,
        sound_from_node, sprite_from_node, timeline_from_node,
    };

    fn resource(kind: ResourceKind, name: &str, source: &str) -> ResourceRef {
        ResourceRef {
            kind,
            index: 0,
            name: name.to_owned(),
            listed_path: name.to_owned(),
            relative_path: PathBuf::from(source),
            source: PathBuf::from(source),
            shader_type: None,
        }
    }

    #[test]
    fn parses_extension_metadata_files_functions_and_constants() {
        let xml = br#"<extension><name>ExampleExt</name><version>1.2.3</version>
            <author>Author</author><date>2026-01-02</date><license>MIT</license>
            <description>Example extension</description><helpfile>help.html</helpfile>
            <installdir>extras</installdir><classname>ExampleClass</classname>
            <androidclassname>AndroidExample</androidclassname><sourcedir>ios</sourcedir>
            <androidsourcedir>android</androidsourcedir><macsourcedir>mac</macsourcedir>
            <maclinkerflags>-framework Metal</maclinkerflags><maccompilerflags>-O2</maccompilerflags>
            <packageID>com.example.ext</packageID><ProductID>00112233445566778899AABBCCDDEEFF</ProductID>
            <androidinject>inject</androidinject><androidmanifestinject>manifest</androidmanifestinject>
            <androidactivityinject>activity</androidactivityinject><gradleinject>gradle</gradleinject>
            <iosplistinject>plist</iosplistinject><iosSystemFrameworks>
            <framework weak="-1">GameKit.framework</framework></iosSystemFrameworks>
            <iosThirdPartyFrameworks><framework weak="0">Vendor.framework</framework></iosThirdPartyFrameworks>
            <ConfigOptions><Config name="Default"><CopyToMask>9223372036854775807</CopyToMask></Config></ConfigOptions>
            <androidPermissions><Permission>android.permission.INTERNET</Permission></androidPermissions>
            <IncludedResources><Resource>Included Files\guide.html</Resource></IncludedResources>
            <files><file><filename>example.dll</filename><origname>extensions\example.dll</origname>
            <init>example_init</init><final>example_done</final><kind>1</kind><uncompress>-1</uncompress>
            <ConfigOptions><Config name="Default"><CopyToMask>64</CopyToMask></Config></ConfigOptions>
            <ProxyFiles><ProxyFile><TargetMask>128</TargetMask><Name>example_x64.dll</Name></ProxyFile></ProxyFiles>
            <functions><function><name>example_call</name><externalName>example_call_raw</externalName>
            <kind>12</kind><help>example_call(value)</help><returnType>2</returnType><argCount>2</argCount>
            <args><arg>1</arg><arg>2</arg></args><hidden>-1</hidden></function></functions>
            <constants><constant><name>EXAMPLE_VALUE</name><value>42</value><hidden>0</hidden></constant></constants>
            </file></files></extension>"#;
        let root = xml::parse(Path::new("ExampleExt.extension.gmx"), Cursor::new(xml)).unwrap();
        let extension = extension_from_node(
            &resource(
                ResourceKind::Extension,
                "ExampleExt",
                "extensions/ExampleExt.extension.gmx",
            ),
            &root,
        )
        .unwrap();

        assert_eq!(extension.name, "ExampleExt");
        assert_eq!(extension.version, "1.2.3");
        assert_eq!(extension.configs[0].copy_to_mask, i64::MAX);
        assert!(extension.ios_system_frameworks[0].weak);
        assert!(!extension.ios_third_party_frameworks[0].weak);
        assert_eq!(
            extension.files[0].source,
            PathBuf::from("extensions/ExampleExt/example.dll")
        );
        assert!(extension.files[0].uncompress);
        assert_eq!(extension.files[0].proxy_files[0].target_mask, 128);
        assert!(extension.enabled_for("Default", 1));
        assert!(extension.files[0].enabled_for("Default", 64));
        assert!(!extension.files[0].enabled_for("Default", 128));
        assert_eq!(
            extension.files[0].filename_for_target(128),
            "example_x64.dll"
        );
        assert_eq!(
            extension.files[0].source_for_target(&extension.folder, 128),
            PathBuf::from("extensions/ExampleExt/example_x64.dll")
        );
        assert_eq!(extension.files[0].functions[0].arguments, [1, 2]);
        assert!(extension.files[0].functions[0].hidden);
        assert_eq!(extension.files[0].constants[0].value, "42");
        assert_eq!(extension.included_resources, ["Included Files\\guide.html"]);
    }

    #[test]
    fn parses_sound_configuration_values() {
        let xml = br#"<sound><kind>3</kind><extension>.ogg</extension>
            <origname>sound\audio\laser.ogg</origname><effects>2</effects>
            <volume><volume>1</volume><volume>0.5</volume></volume><pan>-0.25</pan>
            <bitRates><bitRate>192</bitRate><bitRate>320</bitRate></bitRates>
            <sampleRates><sampleRate>44100</sampleRate><sampleRate>22050</sampleRate></sampleRates>
            <types><type>0</type><type>1</type></types>
            <bitDepths><bitDepth>8</bitDepth><bitDepth>16</bitDepth></bitDepths>
            <preload>-1</preload><compressed>1</compressed><streamed>0</streamed>
            <uncompressOnLoad>true</uncompressOnLoad><audioGroup>2</audioGroup></sound>"#;
        let root = xml::parse(Path::new("laser.sound.gmx"), Cursor::new(xml)).unwrap();
        let sound = sound_from_node(
            &resource(ResourceKind::Sound, "laser", "sound/laser.sound.gmx"),
            &root,
            1,
            true,
        )
        .unwrap();

        assert_eq!(sound.kind, 3);
        assert_eq!(sound.volume, 0.5);
        assert_eq!(sound.bit_rate, 320);
        assert_eq!(sound.sample_rate, 22_050);
        assert!(sound.stereo);
        assert_eq!(sound.bit_depth, 16);
        assert!(sound.preload);
        assert!(sound.compressed);
        assert!(sound.uncompress_on_load);
        assert_eq!(sound.group_index, 2);
    }

    #[test]
    fn parses_background_texture_groups_and_image_path() {
        let xml = br#"<background><istileset>-1</istileset><tilewidth>32</tilewidth>
            <tileheight>16</tileheight><HTile>-1</HTile><VTile>0</VTile>
            <TextureGroups><TextureGroup1>4</TextureGroup1><TextureGroup0>2</TextureGroup0></TextureGroups>
            <For3D>0</For3D><width>64</width><height>32</height>
            <data>images\tiles.png</data></background>"#;
        let root = xml::parse(Path::new("tiles.background.gmx"), Cursor::new(xml)).unwrap();
        let background = background_from_node(
            &resource(
                ResourceKind::Background,
                "tiles",
                "background/tiles.background.gmx",
            ),
            &root,
        )
        .unwrap();

        assert!(background.tileset);
        assert_eq!(background.texture_groups, [2, 4]);
        assert_eq!(
            background.image_source,
            PathBuf::from("background/images/tiles.png")
        );

        let root = xml::parse(
            Path::new("empty.background.gmx"),
            Cursor::new(b"<background/>"),
        )
        .unwrap();
        let background = background_from_node(
            &resource(
                ResourceKind::Background,
                "empty",
                "background/empty.background.gmx",
            ),
            &root,
        )
        .unwrap();
        assert!(background.image_source.as_os_str().is_empty());
    }

    #[test]
    fn parses_path_points_and_boolean_snap_flags() {
        let xml = br#"<path><kind>1</kind><closed>-1</closed><precision>4</precision>
            <backroom>-1</backroom><hsnap>32</hsnap><vsnap>0</vsnap>
            <points><point>1.5,-2,100</point><point>3,4,50.25</point></points></path>"#;
        let root = xml::parse(Path::new("motion.path.gmx"), Cursor::new(xml)).unwrap();
        let path = path_from_node(
            &resource(ResourceKind::Path, "motion", "paths/motion.path.gmx"),
            &root,
        )
        .unwrap();

        assert!(path.closed);
        assert!(path.horizontal_snap);
        assert!(!path.vertical_snap);
        assert_eq!(path.points.len(), 2);
        assert_eq!(path.points[0].x, 1.5);
        assert_eq!(path.points[1].speed, 50.25);
    }

    #[test]
    fn parses_sprite_frames_and_collision_settings() {
        let xml = br#"<sprite><type>0</type><xorig>8</xorig><yorigin>12</yorigin>
            <colkind>5</colkind><coltolerance>7</coltolerance><sepmasks>-1</sepmasks>
            <bboxmode>2</bboxmode><bbox_left>1</bbox_left><bbox_right>14</bbox_right>
            <bbox_top>2</bbox_top><bbox_bottom>15</bbox_bottom><HTile>0</HTile><VTile>-1</VTile>
            <TextureGroups><TextureGroup0>3</TextureGroup0></TextureGroups><For3D>0</For3D>
            <width>16</width><height>18</height><frames>
            <frame index="0">images\hero_0.png</frame><frame index="1">images\hero_1.png</frame>
            </frames></sprite>"#;
        let root = xml::parse(Path::new("hero.sprite.gmx"), Cursor::new(xml)).unwrap();
        let sprite = sprite_from_node(
            &resource(ResourceKind::Sprite, "hero", "sprites/hero.sprite.gmx"),
            &root,
        )
        .unwrap();

        assert_eq!(sprite.sprite_type, SpriteType::Bitmap);
        assert_eq!(sprite.collision_type, CollisionType::RotatedRectangle);
        assert!(sprite.separate_masks);
        assert_eq!(sprite.texture_groups, [3]);
        assert_eq!(sprite.frames.len(), 2);
        assert_eq!(
            sprite.frames[1].source,
            PathBuf::from("sprites/images/hero_1.png")
        );
    }

    #[test]
    fn parses_and_sorts_font_glyphs_and_kerning() {
        let xml = br#"<font><name>Arial</name><size>18</size><bold>-1</bold><renderhq>-1</renderhq>
            <italic>0</italic><charset>1</charset><aa>3</aa><includeTTF>0</includeTTF>
            <TTFName></TTFName><texgroups><texgroup0>2</texgroup0></texgroups>
            <ranges><range0>32,127</range0></ranges><glyphs>
            <glyph character="66" x="10" y="2" w="8" h="12" shift="9" offset="1"/>
            <glyph character="65" x="1" y="2" w="8" h="12" shift="9" offset="0"/>
            </glyphs><kerningPairs><pair first="65" second="86" amount="-1"/></kerningPairs>
            <image>ui.png</image></font>"#;
        let root = xml::parse(Path::new("ui.font.gmx"), Cursor::new(xml)).unwrap();
        let font = font_from_node(
            &resource(ResourceKind::Font, "ui", "fonts/ui.font.gmx"),
            &root,
        )
        .unwrap();

        assert_eq!(font.font_name, "Arial");
        assert_eq!(font.texture_groups, [2]);
        assert_eq!(font.first, 65);
        assert_eq!(font.last, 66);
        assert_eq!(font.glyphs[0].kerning[0].other, 86);
        assert_eq!(font.image_source, PathBuf::from("fonts/ui.png"));
    }

    #[test]
    fn parses_timeline_actions_without_losing_argument_types() {
        let xml = br#"<timeline><entry><step>12</step><event><action>
            <libid>1</libid><id>603</id><kind>7</kind><userelative>0</userelative>
            <isquestion>0</isquestion><useapplyto>-1</useapplyto><exetype>2</exetype>
            <functionname></functionname><codestring>value = 1;</codestring><whoName>self</whoName>
            <relative>0</relative><isnot>0</isnot><arguments><argument>
            <kind>1</kind><string>hello &amp; goodbye</string></argument></arguments>
            </action></event></entry></timeline>"#;
        let root = xml::parse(Path::new("intro.timeline.gmx"), Cursor::new(xml)).unwrap();
        let timeline = timeline_from_node(
            &resource(
                ResourceKind::Timeline,
                "intro",
                "timelines/intro.timeline.gmx",
            ),
            &root,
        )
        .unwrap();

        assert_eq!(timeline.entries[0].step, 12);
        assert!(timeline.entries[0].actions[0].use_apply_to);
        assert_eq!(timeline.entries[0].actions[0].arguments[0].kind, 1);
        assert_eq!(
            timeline.entries[0].actions[0].arguments[0].value,
            "hello & goodbye"
        );
    }

    #[test]
    fn parses_object_events_and_physics_shape() {
        let xml = br#"<object><spriteName>hero</spriteName><solid>-1</solid><visible>-1</visible>
            <depth>-5</depth><persistent>0</persistent><parentName>base</parentName>
            <maskName>&lt;undefined&gt;</maskName><events>
            <event eventtype="4" ename="enemy"><action><libid>1</libid><id>603</id>
            <kind>7</kind><useapplyto>-1</useapplyto><exetype>2</exetype><whoName>other</whoName>
            <arguments><argument><kind>1</kind><string>hp -= 1;</string></argument></arguments>
            </action></event></events><PhysicsObject>-1</PhysicsObject>
            <PhysicsObjectSensor>0</PhysicsObjectSensor><PhysicsObjectShape>1</PhysicsObjectShape>
            <PhysicsObjectDensity>0.5</PhysicsObjectDensity><PhysicsObjectRestitution>0.1</PhysicsObjectRestitution>
            <PhysicsObjectGroup>2</PhysicsObjectGroup><PhysicsObjectLinearDamping>0.2</PhysicsObjectLinearDamping>
            <PhysicsObjectAngularDamping>0.3</PhysicsObjectAngularDamping>
            <PhysicsObjectFriction>0.4</PhysicsObjectFriction><PhysicsObjectAwake>-1</PhysicsObjectAwake>
            <PhysicsObjectKinematic>0</PhysicsObjectKinematic>
            <PhysicsShapePoints><point>10.5,20</point><point>30,40.25</point></PhysicsShapePoints>
            </object>"#;
        let root = xml::parse(Path::new("player.object.gmx"), Cursor::new(xml)).unwrap();
        let object = object_from_node(
            &resource(ResourceKind::Object, "player", "objects/player.object.gmx"),
            &root,
        )
        .unwrap();

        assert_eq!(object.events.len(), OBJECT_EVENT_TYPE_COUNT);
        assert_eq!(object.events[4][0].subtype, OBJECT_UNDEFINED);
        assert_eq!(object.events[4][0].subtype_name.as_deref(), Some("enemy"));
        assert_eq!(object.events[4][0].actions[0].who, -2);
        assert_eq!(object.physics_shape_points[0].x, 10.5);
        assert_eq!(object.physics_shape_points[1].y, 40.25);
        assert!(object.physics_object);
    }

    #[test]
    fn parses_room_layers_instances_tiles_and_physics() {
        let xml = br#"<room><caption>Stage One</caption><width>640</width><height>480</height>
            <vsnap>16</vsnap><hsnap>32</hsnap><isometric>0</isometric><speed>60</speed>
            <persistent>-1</persistent><colour>12632256</colour><showcolour>-1</showcolour>
            <code>room_started = true;</code><enableViews>-1</enableViews>
            <clearViewBackground>-1</clearViewBackground><clearDisplayBuffer>0</clearDisplayBuffer>
            <makerSettings><isSet>-1</isSet><w>900</w><h>700</h><showGrid>-1</showGrid>
            <showObjects>-1</showObjects><showTiles>-1</showTiles><showBackgrounds>-1</showBackgrounds>
            <showForegrounds>0</showForegrounds><showViews>-1</showViews><deleteUnderlyingObj>0</deleteUnderlyingObj>
            <deleteUnderlyingTiles>-1</deleteUnderlyingTiles><page>2</page><xoffset>10</xoffset><yoffset>20</yoffset>
            </makerSettings><backgrounds><background visible="-1" foreground="0" name="bSky" x="1" y="2"
            htiled="-1" vtiled="0" hspeed="3" vspeed="4" stretch="0"/></backgrounds>
            <views><view visible="-1" objName="&lt;undefined&gt;" xview="5" yview="6" wview="320" hview="240"
            xport="7" yport="8" wport="640" hport="480" hborder="64" vborder="48" hspeed="-1" vspeed="-1"/></views>
            <instances><instance objName="oPlayer" x="100" y="200" name="inst_ABCDEF01" locked="0"
            code="line1&#xA;line2" scaleX="1.5" scaleY="0.5" colour="4279312947" rotation="45"/></instances>
            <tiles><tile bgName="bTiles" x="10" y="20" w="32" h="16" xo="64" yo="80" id="10000042"
            name="inst_1234ABCD" depth="1000" locked="-1" colour="2148606515" scaleX="2" scaleY="3"/></tiles>
            <PhysicsWorld>-1</PhysicsWorld><PhysicsWorldTop>-10</PhysicsWorldTop><PhysicsWorldLeft>-20</PhysicsWorldLeft>
            <PhysicsWorldRight>660</PhysicsWorldRight><PhysicsWorldBottom>500</PhysicsWorldBottom>
            <PhysicsWorldGravityX>1.25</PhysicsWorldGravityX><PhysicsWorldGravityY>9.8</PhysicsWorldGravityY>
            <PhysicsWorldPixToMeters>0.1</PhysicsWorldPixToMeters></room>"#;
        let root = xml::parse(Path::new("stage.room.gmx"), Cursor::new(xml)).unwrap();
        let room = room_from_node(
            &resource(ResourceKind::Room, "stage", "rooms/stage.room.gmx"),
            &root,
        )
        .unwrap();

        assert_eq!(room.caption, "Stage One");
        assert_eq!((room.width, room.height), (640, 480));
        assert!(room.persistent);
        assert!(room.enable_views);
        assert_eq!(room.maker_settings.as_ref().unwrap().page, 2);
        assert_eq!(room.backgrounds[0].background_name, "bSky");
        assert_eq!(room.views[0].object_name, "<undefined>");
        assert_eq!(room.instances[0].code, "line1\nline2");
        assert_eq!(room.instances[0].color, 0xff11_2233);
        assert_eq!(room.instances[0].wad_color(), 0xff33_2211);
        assert_eq!(room.tiles[0].listed_id, 10_000_042);
        assert_eq!(room.tiles[0].blend, 0x0033_2211);
        assert_eq!(room.tiles[0].alpha, 128.0 / 255.0);
        assert!(room.tiles[0].locked);
        assert!(room.physics_world);
        assert_eq!(room.physics_world_gravity_x, 1.25);
    }
}
