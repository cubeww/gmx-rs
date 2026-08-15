use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::encoding::Decoder;
use quick_xml::events::{BytesStart, Event};

use crate::path::{gmx_path, push_gmx_path};
use crate::xml::{attribute, resolve_reference};

const RESOURCE_KIND_COUNT: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum ResourceKind {
    Config,
    Extension,
    Sound,
    Sprite,
    Background,
    Path,
    Script,
    Shader,
    Font,
    Timeline,
    Object,
    Room,
}

impl ResourceKind {
    pub const ALL: [Self; RESOURCE_KIND_COUNT] = [
        Self::Config,
        Self::Extension,
        Self::Sound,
        Self::Sprite,
        Self::Background,
        Self::Path,
        Self::Script,
        Self::Shader,
        Self::Font,
        Self::Timeline,
        Self::Object,
        Self::Room,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Config => "config",
            Self::Extension => "extension",
            Self::Sound => "sound",
            Self::Sprite => "sprite",
            Self::Background => "background",
            Self::Path => "path",
            Self::Script => "script",
            Self::Shader => "shader",
            Self::Font => "font",
            Self::Timeline => "timeline",
            Self::Object => "object",
            Self::Room => "room",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRef {
    pub kind: ResourceKind,
    pub index: usize,
    pub name: String,
    pub listed_path: String,
    pub relative_path: PathBuf,
    pub source: PathBuf,
    pub shader_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataFileConfig {
    pub name: String,
    pub copy_to_mask: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataFile {
    pub name: String,
    pub listed_filename: Option<String>,
    pub relative_path: PathBuf,
    pub source: PathBuf,
    pub exists: bool,
    pub size: u64,
    pub export_action: i32,
    pub export_dir: String,
    pub overwrite: bool,
    pub free_data: bool,
    pub remove_end: bool,
    pub store: bool,
    pub configs: Vec<DataFileConfig>,
}

impl DataFile {
    pub fn enabled_for(&self, config: &str, target_mask: i64) -> bool {
        self.configs
            .iter()
            .find(|entry| entry.name == config)
            .is_none_or(|entry| (entry.copy_to_mask & target_mask) != 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConstant {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioGroup {
    pub index: usize,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectManifest {
    pub name: String,
    pub project_file: PathBuf,
    pub root_dir: PathBuf,
    pub resources: Vec<ResourceRef>,
    pub data_files: Vec<DataFile>,
    pub constants: Vec<ProjectConstant>,
    pub audio_groups: Vec<AudioGroup>,
}

impl ProjectManifest {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| LoadError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        parse(path, BufReader::new(file))
    }

    pub fn resources_of(&self, kind: ResourceKind) -> impl Iterator<Item = &ResourceRef> + '_ {
        self.resources
            .iter()
            .filter(move |resource| resource.kind == kind)
    }

    pub fn resource_count(&self, kind: ResourceKind) -> usize {
        self.resources_of(kind).count()
    }
}

#[derive(Debug)]
pub enum LoadError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Xml {
        path: PathBuf,
        offset: u64,
        message: String,
    },
    InvalidRoot {
        path: PathBuf,
        actual: String,
    },
    MissingRoot {
        path: PathBuf,
    },
    EmptyResourcePath {
        path: PathBuf,
        kind: ResourceKind,
    },
    InvalidField {
        path: PathBuf,
        field: &'static str,
        value: String,
    },
    MissingDataFileName {
        path: PathBuf,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::Xml {
                path,
                offset,
                message,
            } => write!(
                formatter,
                "invalid XML in {} near byte {offset}: {message}",
                path.display()
            ),
            Self::InvalidRoot { path, actual } => write!(
                formatter,
                "invalid project root {actual:?} in {}; expected assets",
                path.display()
            ),
            Self::MissingRoot { path } => {
                write!(
                    formatter,
                    "{} does not contain an assets root",
                    path.display()
                )
            }
            Self::EmptyResourcePath { path, kind } => {
                write!(formatter, "empty {kind} path in project {}", path.display())
            }
            Self::InvalidField { path, field, value } => write!(
                formatter,
                "invalid {field} value {value:?} in project {}",
                path.display()
            ),
            Self::MissingDataFileName { path } => write!(
                formatter,
                "datafile without a name in project {}",
                path.display()
            ),
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn parse(path: &Path, input: impl BufRead) -> Result<ProjectManifest, LoadError> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(true);
    reader.config_mut().expand_empty_elements = true;

    let mut parser = ManifestParser::new(path);
    let mut buffer = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| parser.xml_error(reader.buffer_position(), error))?;
        match event {
            Event::Start(start) => {
                parser.start(&start, reader.decoder(), reader.buffer_position())?
            }
            Event::End(end) => parser.end(end.name().as_ref(), reader.buffer_position())?,
            Event::Text(text) => {
                if parser.capture.is_some() {
                    let text = text
                        .xml10_content()
                        .map_err(|error| parser.xml_error(reader.buffer_position(), error))?;
                    parser.append_text(&text);
                }
            }
            Event::CData(text) => {
                if parser.capture.is_some() {
                    let text = text
                        .decode()
                        .map_err(|error| parser.xml_error(reader.buffer_position(), error))?;
                    parser.append_text(&text);
                }
            }
            Event::GeneralRef(reference) => {
                if parser.capture.is_some() {
                    let value = resolve_reference(&reference)
                        .map_err(|error| parser.xml_error(reader.buffer_position(), error))?;
                    parser.append_text(&value);
                }
            }
            Event::Eof => break,
            Event::Decl(_)
            | Event::PI(_)
            | Event::Comment(_)
            | Event::DocType(_)
            | Event::Empty(_) => {}
        }
        buffer.clear();
    }

    parser.finish()
}

struct ManifestParser {
    project_file: PathBuf,
    root_dir: PathBuf,
    project_name: String,
    resources: Vec<ResourceRef>,
    resource_counts: [usize; RESOURCE_KIND_COUNT],
    data_files: Vec<DataFile>,
    constants: Vec<ProjectConstant>,
    audio_groups: Vec<AudioGroup>,
    stack: Vec<Vec<u8>>,
    data_file_groups: Vec<String>,
    data_file: Option<DataFileBuilder>,
    capture: Option<TextCapture>,
    root_seen: bool,
}

impl ManifestParser {
    fn new(path: &Path) -> Self {
        Self {
            project_file: path.to_path_buf(),
            root_dir: path.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
            project_name: project_name(path),
            resources: Vec::new(),
            resource_counts: [0; RESOURCE_KIND_COUNT],
            data_files: Vec::new(),
            constants: Vec::new(),
            audio_groups: Vec::new(),
            stack: Vec::new(),
            data_file_groups: Vec::new(),
            data_file: None,
            capture: None,
            root_seen: false,
        }
    }

    fn start(
        &mut self,
        start: &BytesStart<'_>,
        decoder: Decoder,
        offset: u64,
    ) -> Result<(), LoadError> {
        let name = start.name().as_ref().to_vec();
        if self.stack.is_empty() {
            if self.root_seen || name.as_slice() != b"assets" {
                return Err(LoadError::InvalidRoot {
                    path: self.project_file.clone(),
                    actual: String::from_utf8_lossy(&name).into_owned(),
                });
            }
            self.root_seen = true;
        }

        let parent = self.stack.last().map(Vec::as_slice);
        let section = self.stack.get(1).map(Vec::as_slice);

        if name.as_slice() == b"datafiles" && section.is_none_or(|value| value == b"datafiles") {
            let group = attribute(start, b"name", decoder)
                .map_err(|message| self.xml_error(offset, message))?
                .unwrap_or_default();
            self.data_file_groups.push(group);
        } else if name.as_slice() == b"datafile" && section == Some(b"datafiles") {
            if self.data_file.is_some() {
                return Err(self.xml_error(offset, "nested datafile elements are not supported"));
            }
            self.data_file = Some(DataFileBuilder::new(self.data_file_groups.clone()));
        } else if let Some(kind) = resource_kind(section, &name) {
            let shader_type = if kind == ResourceKind::Shader {
                Some(
                    attribute(start, b"type", decoder)
                        .map_err(|message| self.xml_error(offset, message))?
                        .unwrap_or_else(|| "GLSLES".to_owned()),
                )
            } else {
                None
            };
            self.begin_capture(
                name.clone(),
                CaptureTarget::Resource { kind, shader_type },
                offset,
            )?;
        } else if section == Some(b"constants") && name.as_slice() == b"constant" {
            let constant_name = attribute(start, b"name", decoder)
                .map_err(|message| self.xml_error(offset, message))?
                .unwrap_or_default();
            self.begin_capture(
                name.clone(),
                CaptureTarget::Constant {
                    name: constant_name,
                },
                offset,
            )?;
        } else if section == Some(b"audiogroups") && name.as_slice() == b"audiogroup" {
            let group_name = attribute(start, b"name", decoder)
                .map_err(|message| self.xml_error(offset, message))?
                .unwrap_or_default();
            self.audio_groups.push(AudioGroup {
                index: self.audio_groups.len(),
                name: group_name,
            });
        } else if self.data_file.is_some() {
            if parent == Some(b"datafile") {
                if let Some(field) = DataField::from_element(&name) {
                    self.begin_capture(name.clone(), CaptureTarget::DataField(field), offset)?;
                }
            } else if parent == Some(b"ConfigOptions") && name.as_slice() == b"Config" {
                let config_name = attribute(start, b"name", decoder)
                    .map_err(|message| self.xml_error(offset, message))?
                    .unwrap_or_default();
                self.data_file.as_mut().unwrap().current_config = Some(config_name);
            } else if parent == Some(b"Config") && name.as_slice() == b"CopyToMask" {
                let config_name = self
                    .data_file
                    .as_ref()
                    .and_then(|data_file| data_file.current_config.clone())
                    .unwrap_or_default();
                self.begin_capture(
                    name.clone(),
                    CaptureTarget::CopyToMask {
                        config: config_name,
                    },
                    offset,
                )?;
            }
        }

        self.stack.push(name);
        Ok(())
    }

    fn end(&mut self, name: &[u8], offset: u64) -> Result<(), LoadError> {
        if self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.element == name)
        {
            let capture = self.capture.take().unwrap();
            self.finish_capture(capture)?;
        }

        if name == b"Config" {
            if let Some(data_file) = &mut self.data_file {
                data_file.current_config = None;
            }
        } else if name == b"datafile" {
            let data_file = self
                .data_file
                .take()
                .ok_or_else(|| self.xml_error(offset, "closing datafile without an opening tag"))?;
            self.data_files
                .push(data_file.finish(&self.project_file, &self.root_dir)?);
        } else if name == b"datafiles" {
            self.data_file_groups
                .pop()
                .ok_or_else(|| self.xml_error(offset, "unbalanced datafiles element"))?;
        }

        let open = self
            .stack
            .pop()
            .ok_or_else(|| self.xml_error(offset, "closing tag without an opening tag"))?;
        if open != name {
            return Err(self.xml_error(
                offset,
                format!(
                    "closing tag {} does not match {}",
                    String::from_utf8_lossy(name),
                    String::from_utf8_lossy(&open)
                ),
            ));
        }
        Ok(())
    }

    fn append_text(&mut self, text: &str) {
        if let Some(capture) = &mut self.capture {
            capture.text.push_str(text);
        }
    }

    fn begin_capture(
        &mut self,
        element: Vec<u8>,
        target: CaptureTarget,
        offset: u64,
    ) -> Result<(), LoadError> {
        if self.capture.is_some() {
            return Err(self.xml_error(offset, "nested text fields are not supported"));
        }
        self.capture = Some(TextCapture {
            element,
            target,
            text: String::new(),
        });
        Ok(())
    }

    fn finish_capture(&mut self, capture: TextCapture) -> Result<(), LoadError> {
        let value = capture.text.trim();
        match capture.target {
            CaptureTarget::Resource { kind, shader_type } => {
                if value.is_empty() {
                    return Err(LoadError::EmptyResourcePath {
                        path: self.project_file.clone(),
                        kind,
                    });
                }
                let relative_path = resource_path(kind, value);
                let index = self.resource_counts[kind.index()];
                self.resource_counts[kind.index()] += 1;
                self.resources.push(ResourceRef {
                    kind,
                    index,
                    name: resource_name(kind, value),
                    listed_path: value.to_owned(),
                    source: self.root_dir.join(&relative_path),
                    relative_path,
                    shader_type,
                });
            }
            CaptureTarget::Constant { name } => self.constants.push(ProjectConstant {
                name,
                value: value.to_owned(),
            }),
            CaptureTarget::DataField(field) => {
                let path = self.project_file.clone();
                let data_file = self.data_file.as_mut().unwrap();
                field.set(data_file, value, &path)?;
            }
            CaptureTarget::CopyToMask { config } => {
                let copy_to_mask = parse_field(value, "CopyToMask", &self.project_file)?;
                self.data_file
                    .as_mut()
                    .unwrap()
                    .configs
                    .push(DataFileConfig {
                        name: config,
                        copy_to_mask,
                    });
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<ProjectManifest, LoadError> {
        if !self.root_seen {
            return Err(LoadError::MissingRoot {
                path: self.project_file,
            });
        }
        if !self.stack.is_empty() || self.capture.is_some() || self.data_file.is_some() {
            return Err(self.xml_error(0, "project XML ended with unclosed elements"));
        }
        Ok(ProjectManifest {
            name: self.project_name,
            project_file: self.project_file,
            root_dir: self.root_dir,
            resources: self.resources,
            data_files: self.data_files,
            constants: self.constants,
            audio_groups: self.audio_groups,
        })
    }

    fn xml_error(&self, offset: u64, error: impl fmt::Display) -> LoadError {
        LoadError::Xml {
            path: self.project_file.clone(),
            offset,
            message: error.to_string(),
        }
    }
}

struct TextCapture {
    element: Vec<u8>,
    target: CaptureTarget,
    text: String,
}

enum CaptureTarget {
    Resource {
        kind: ResourceKind,
        shader_type: Option<String>,
    },
    Constant {
        name: String,
    },
    DataField(DataField),
    CopyToMask {
        config: String,
    },
}

#[derive(Clone, Copy)]
enum DataField {
    Name,
    Exists,
    Size,
    ExportAction,
    ExportDir,
    Overwrite,
    FreeData,
    RemoveEnd,
    Store,
    Filename,
}

impl DataField {
    fn from_element(element: &[u8]) -> Option<Self> {
        match element {
            b"name" => Some(Self::Name),
            b"exists" => Some(Self::Exists),
            b"size" => Some(Self::Size),
            b"exportAction" => Some(Self::ExportAction),
            b"exportDir" => Some(Self::ExportDir),
            b"overwrite" => Some(Self::Overwrite),
            b"freeData" => Some(Self::FreeData),
            b"removeEnd" => Some(Self::RemoveEnd),
            b"store" => Some(Self::Store),
            b"filename" => Some(Self::Filename),
            _ => None,
        }
    }

    fn set(
        self,
        data_file: &mut DataFileBuilder,
        value: &str,
        path: &Path,
    ) -> Result<(), LoadError> {
        match self {
            Self::Name => data_file.name = Some(value.to_owned()),
            Self::Exists => data_file.exists = parse_bool(value, "exists", path)?,
            Self::Size => data_file.size = parse_field(value, "size", path)?,
            Self::ExportAction => {
                data_file.export_action = parse_field(value, "exportAction", path)?
            }
            Self::ExportDir => data_file.export_dir = value.to_owned(),
            Self::Overwrite => data_file.overwrite = parse_bool(value, "overwrite", path)?,
            Self::FreeData => data_file.free_data = parse_bool(value, "freeData", path)?,
            Self::RemoveEnd => data_file.remove_end = parse_bool(value, "removeEnd", path)?,
            Self::Store => data_file.store = parse_bool(value, "store", path)?,
            Self::Filename => data_file.listed_filename = Some(value.to_owned()),
        }
        Ok(())
    }
}

#[derive(Default)]
struct DataFileBuilder {
    groups: Vec<String>,
    name: Option<String>,
    listed_filename: Option<String>,
    exists: bool,
    size: u64,
    export_action: i32,
    export_dir: String,
    overwrite: bool,
    free_data: bool,
    remove_end: bool,
    store: bool,
    configs: Vec<DataFileConfig>,
    current_config: Option<String>,
}

impl DataFileBuilder {
    fn new(groups: Vec<String>) -> Self {
        Self {
            groups,
            ..Self::default()
        }
    }

    fn finish(self, project_file: &Path, root_dir: &Path) -> Result<DataFile, LoadError> {
        let name = self.name.filter(|name| !name.is_empty()).ok_or_else(|| {
            LoadError::MissingDataFileName {
                path: project_file.to_path_buf(),
            }
        })?;
        let mut relative_path = PathBuf::new();
        for group in self.groups.iter().filter(|group| !group.is_empty()) {
            push_gmx_path(&mut relative_path, group);
        }
        push_gmx_path(&mut relative_path, &name);

        Ok(DataFile {
            name,
            listed_filename: self.listed_filename,
            source: root_dir.join(&relative_path),
            relative_path,
            exists: self.exists,
            size: self.size,
            export_action: self.export_action,
            export_dir: self.export_dir,
            overwrite: self.overwrite,
            free_data: self.free_data,
            remove_end: self.remove_end,
            store: self.store,
            configs: self.configs,
        })
    }
}

fn resource_kind(section: Option<&[u8]>, element: &[u8]) -> Option<ResourceKind> {
    match (section, element) {
        (Some(b"Configs"), b"Config") => Some(ResourceKind::Config),
        (Some(b"NewExtensions"), b"extension") => Some(ResourceKind::Extension),
        (Some(b"sounds"), b"sound") => Some(ResourceKind::Sound),
        (Some(b"sprites"), b"sprite") => Some(ResourceKind::Sprite),
        (Some(b"backgrounds"), b"background") => Some(ResourceKind::Background),
        (Some(b"paths"), b"path") => Some(ResourceKind::Path),
        (Some(b"scripts"), b"script") => Some(ResourceKind::Script),
        (Some(b"shaders"), b"shader") => Some(ResourceKind::Shader),
        (Some(b"fonts"), b"font") => Some(ResourceKind::Font),
        (Some(b"timelines"), b"timeline") => Some(ResourceKind::Timeline),
        (Some(b"objects"), b"object") => Some(ResourceKind::Object),
        (Some(b"rooms"), b"room") => Some(ResourceKind::Room),
        _ => None,
    }
}

fn resource_path(kind: ResourceKind, listed_path: &str) -> PathBuf {
    if kind == ResourceKind::Extension {
        return gmx_path(&format!("{listed_path}.extension.gmx"));
    }

    let extension = match kind {
        ResourceKind::Config => "config.gmx",
        ResourceKind::Sound => "sound.gmx",
        ResourceKind::Sprite => "sprite.gmx",
        ResourceKind::Background => "background.gmx",
        ResourceKind::Path => "path.gmx",
        ResourceKind::Script => "gml",
        ResourceKind::Shader => "shader",
        ResourceKind::Font => "font.gmx",
        ResourceKind::Timeline => "timeline.gmx",
        ResourceKind::Object => "object.gmx",
        ResourceKind::Room => "room.gmx",
        ResourceKind::Extension => unreachable!(),
    };
    let mut path = gmx_path(listed_path);
    path.set_extension(extension);
    path
}

fn resource_name(kind: ResourceKind, listed_path: &str) -> String {
    let file_name = listed_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(listed_path);
    if matches!(
        kind,
        ResourceKind::Config
            | ResourceKind::Extension
            | ResourceKind::Script
            | ResourceKind::Shader
    ) {
        file_name
            .rsplit_once('.')
            .map_or(file_name, |(stem, _)| stem)
            .to_owned()
    } else {
        file_name.to_owned()
    }
}

fn project_name(path: &Path) -> String {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    file_name
        .strip_suffix(".project.gmx")
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or(file_name)
        })
        .to_owned()
}

fn parse_bool(value: &str, field: &'static str, path: &Path) -> Result<bool, LoadError> {
    if let Ok(number) = value.parse::<i64>() {
        return Ok(number != 0);
    }
    match value.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(LoadError::InvalidField {
            path: path.to_path_buf(),
            field,
            value: value.to_owned(),
        }),
    }
}

fn parse_field<T>(value: &str, field: &'static str, path: &Path) -> Result<T, LoadError>
where
    T: std::str::FromStr,
{
    value.parse().map_err(|_| LoadError::InvalidField {
        path: path.to_path_buf(),
        field,
        value: value.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::{Path, PathBuf};

    use super::{LoadError, ProjectManifest, ResourceKind, parse};

    const PROJECT: &str = r#"
        <assets>
          <Configs name="configs">
            <Config>Configs\Default</Config>
          </Configs>
          <datafiles number="1" name="datafiles">
            <datafiles number="1" name="Music">
              <datafile>
                <name>theme.ogg</name>
                <exists>-1</exists>
                <size>123</size>
                <exportAction>2</exportAction>
                <exportDir></exportDir>
                <overwrite>0</overwrite>
                <freeData>-1</freeData>
                <removeEnd>0</removeEnd>
                <store>0</store>
                <ConfigOptions>
                  <Config name="Default"><CopyToMask>9223372036854775807</CopyToMask></Config>
                </ConfigOptions>
                <filename>theme.ogg</filename>
              </datafile>
            </datafiles>
          </datafiles>
          <sounds name="sounds">
            <sounds name="folder">
              <sound>sound\snd&amp;One</sound>
              <sound>sound/sndTwo</sound>
            </sounds>
          </sounds>
          <scripts name="scripts"><script>scripts\start.gml</script></scripts>
          <shaders name="shaders">
            <shader type="GLSLES">shaders\basic.shader</shader>
          </shaders>
          <constants><constant name="ANSWER">40 + 2</constant></constants>
          <audiogroups><audiogroup name="music"/></audiogroups>
        </assets>
    "#;

    fn load_project() -> ProjectManifest {
        parse(
            Path::new("game/Test.project.gmx"),
            Cursor::new(PROJECT.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn reads_resources_in_manifest_order() {
        let project = load_project();

        assert_eq!(project.name, "Test");
        assert_eq!(project.resources.len(), 5);
        assert_eq!(project.resource_count(ResourceKind::Sound), 2);
        assert_eq!(project.resources[1].name, "snd&One");
        assert_eq!(project.resources[1].index, 0);
        assert_eq!(project.resources[2].name, "sndTwo");
        assert_eq!(project.resources[2].index, 1);
        assert_eq!(
            project.resources[1].relative_path,
            PathBuf::from("sound").join("snd&One.sound.gmx")
        );
        assert_eq!(
            project.resources[3].relative_path,
            PathBuf::from("scripts").join("start.gml")
        );
        assert_eq!(project.resources[4].shader_type.as_deref(), Some("GLSLES"));
    }

    #[test]
    fn reads_datafiles_constants_and_audio_groups() {
        let project = load_project();

        assert_eq!(project.data_files.len(), 1);
        let data_file = &project.data_files[0];
        assert_eq!(data_file.name, "theme.ogg");
        assert_eq!(
            data_file.relative_path,
            PathBuf::from("datafiles").join("Music").join("theme.ogg")
        );
        assert!(data_file.exists);
        assert_eq!(data_file.size, 123);
        assert!(data_file.free_data);
        assert_eq!(data_file.configs[0].name, "Default");
        assert_eq!(data_file.configs[0].copy_to_mask, i64::MAX);
        assert_eq!(project.constants[0].name, "ANSWER");
        assert_eq!(project.constants[0].value, "40 + 2");
        assert_eq!(project.audio_groups[0].name, "music");
    }

    #[test]
    fn rejects_non_project_xml() {
        let error = parse(Path::new("broken.project.gmx"), Cursor::new(b"<project/>")).unwrap_err();
        assert!(matches!(error, LoadError::InvalidRoot { .. }));
    }
}
