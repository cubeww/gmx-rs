use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::project::{ProjectManifest, ResourceKind};
use crate::xml::{attribute, resolve_reference};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub source: PathBuf,
    options: Vec<ConfigEntry>,
    constants: Vec<ConfigEntry>,
    option_index: HashMap<String, usize>,
    constant_index: HashMap<String, usize>,
}

impl Config {
    pub fn load(name: impl Into<String>, path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        parse(name.into(), path, BufReader::new(file))
    }

    pub fn load_from_project(project: &ProjectManifest, name: &str) -> Result<Self, ConfigError> {
        let resource = project
            .resources_of(ResourceKind::Config)
            .find(|resource| resource.name == name)
            .ok_or_else(|| ConfigError::NotFound {
                project: project.project_file.clone(),
                name: name.to_owned(),
            })?;
        Self::load(name, &resource.source)
    }

    pub fn options(&self) -> &[ConfigEntry] {
        &self.options
    }

    pub fn constants(&self) -> &[ConfigEntry] {
        &self.constants
    }

    pub fn option(&self, name: &str) -> Option<&str> {
        self.option_index
            .get(name)
            .map(|index| self.options[*index].value.as_str())
    }

    pub fn constant(&self, name: &str) -> Option<&str> {
        self.constant_index
            .get(name)
            .map(|index| self.constants[*index].value.as_str())
    }
}

#[derive(Debug)]
pub enum ConfigError {
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
    MissingConstantName {
        path: PathBuf,
    },
    DuplicateOption {
        path: PathBuf,
        name: String,
    },
    DuplicateConstant {
        path: PathBuf,
        name: String,
    },
    NotFound {
        project: PathBuf,
        name: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot read config {}: {source}", path.display())
            }
            Self::Xml {
                path,
                offset,
                message,
            } => write!(
                formatter,
                "invalid XML in config {} near byte {offset}: {message}",
                path.display()
            ),
            Self::InvalidRoot { path, actual } => write!(
                formatter,
                "invalid config root {actual:?} in {}; expected Config",
                path.display()
            ),
            Self::MissingRoot { path } => {
                write!(
                    formatter,
                    "{} does not contain a Config root",
                    path.display()
                )
            }
            Self::MissingConstantName { path } => write!(
                formatter,
                "configuration constant without a name in {}",
                path.display()
            ),
            Self::DuplicateOption { path, name } => write!(
                formatter,
                "duplicate option {name:?} in config {}",
                path.display()
            ),
            Self::DuplicateConstant { path, name } => write!(
                formatter,
                "duplicate constant {name:?} in config {}",
                path.display()
            ),
            Self::NotFound { project, name } => write!(
                formatter,
                "configuration {name:?} does not exist in project {}",
                project.display()
            ),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn parse(name: String, path: &Path, input: impl BufRead) -> Result<Config, ConfigError> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().expand_empty_elements = true;

    let mut stack: Vec<Node> = Vec::new();
    let mut root_options = Vec::new();
    let mut nested_options = Vec::new();
    let mut constants = Vec::new();
    let mut root_seen = false;
    let mut buffer = Vec::new();

    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| xml_error(path, reader.buffer_position(), error))?;
        match event {
            Event::Start(start) => {
                let element = start.name().as_ref().to_vec();
                if stack.is_empty() {
                    if root_seen || element.as_slice() != b"Config" {
                        return Err(ConfigError::InvalidRoot {
                            path: path.to_path_buf(),
                            actual: String::from_utf8_lossy(&element).into_owned(),
                        });
                    }
                    root_seen = true;
                }

                let constant_name =
                    if path_is(&stack, &[b"Config", b"ConfigConstants", b"constants"]) {
                        attribute(&start, b"name", reader.decoder())
                            .map_err(|error| xml_error(path, reader.buffer_position(), error))?
                    } else {
                        None
                    };
                stack.push(Node {
                    name: element,
                    text: String::new(),
                    constant_name,
                });
            }
            Event::End(end) => {
                let element = end.name().as_ref().to_vec();
                let node = stack.pop().ok_or_else(|| {
                    xml_error(
                        path,
                        reader.buffer_position(),
                        "closing tag without an opening tag",
                    )
                })?;
                if node.name != element {
                    return Err(xml_error(
                        path,
                        reader.buffer_position(),
                        "closing tag does not match its opening tag",
                    ));
                }

                if path_is(&stack, &[b"Config", b"ConfigConstants", b"constants"]) {
                    let constant_name =
                        node.constant_name
                            .ok_or_else(|| ConfigError::MissingConstantName {
                                path: path.to_path_buf(),
                            })?;
                    constants.push(ConfigEntry {
                        name: constant_name,
                        value: node.text,
                    });
                } else if path_is(&stack, &[b"Config", b"Options"]) {
                    nested_options.push(ConfigEntry {
                        name: String::from_utf8_lossy(&node.name).into_owned(),
                        value: node.text,
                    });
                } else if path_is(&stack, &[b"Config"]) {
                    root_options.push(ConfigEntry {
                        name: String::from_utf8_lossy(&node.name).into_owned(),
                        value: node.text,
                    });
                }
            }
            Event::Text(text) => {
                let text = text
                    .xml10_content()
                    .map_err(|error| xml_error(path, reader.buffer_position(), error))?;
                append_text(&mut stack, &text);
            }
            Event::CData(text) => {
                let text = text
                    .decode()
                    .map_err(|error| xml_error(path, reader.buffer_position(), error))?;
                append_text(&mut stack, &text);
            }
            Event::GeneralRef(reference) => {
                let text = resolve_reference(&reference)
                    .map_err(|error| xml_error(path, reader.buffer_position(), error))?;
                append_text(&mut stack, &text);
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

    if !root_seen {
        return Err(ConfigError::MissingRoot {
            path: path.to_path_buf(),
        });
    }
    if !stack.is_empty() {
        return Err(xml_error(
            path,
            0,
            "config XML ended with unclosed elements",
        ));
    }

    let options = if nested_options.is_empty() {
        root_options
    } else {
        nested_options
    };
    let option_index = make_index(&options, path, false)?;
    let constant_index = make_index(&constants, path, true)?;

    Ok(Config {
        name,
        source: path.to_path_buf(),
        options,
        constants,
        option_index,
        constant_index,
    })
}

struct Node {
    name: Vec<u8>,
    text: String,
    constant_name: Option<String>,
}

fn path_is(stack: &[Node], path: &[&[u8]]) -> bool {
    stack.len() == path.len()
        && stack
            .iter()
            .zip(path)
            .all(|(node, expected)| node.name == *expected)
}

fn append_text(stack: &mut [Node], text: &str) {
    for node in stack {
        node.text.push_str(text);
    }
}

fn make_index(
    entries: &[ConfigEntry],
    path: &Path,
    constants: bool,
) -> Result<HashMap<String, usize>, ConfigError> {
    let mut index = HashMap::with_capacity(entries.len());
    for (position, entry) in entries.iter().enumerate() {
        if index.insert(entry.name.clone(), position).is_some() {
            return Err(if constants {
                ConfigError::DuplicateConstant {
                    path: path.to_path_buf(),
                    name: entry.name.clone(),
                }
            } else {
                ConfigError::DuplicateOption {
                    path: path.to_path_buf(),
                    name: entry.name.clone(),
                }
            });
        }
    }
    Ok(index)
}

fn xml_error(path: &Path, offset: u64, error: impl fmt::Display) -> ConfigError {
    ConfigError::Xml {
        path: path.to_path_buf(),
        offset,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use super::{ConfigError, parse};

    #[test]
    fn reads_options_and_constants() {
        let xml = br#"
            <Config>
              <Options>
                <option_gameid>42</option_gameid>
                <option_empty></option_empty>
                <option_name>Rock &amp; Roll</option_name>
              </Options>
              <ConfigConstants>
                <constants><constant name="LIMIT">100</constant></constants>
              </ConfigConstants>
            </Config>
        "#;
        let config = parse(
            "Default".to_owned(),
            Path::new("Default.config.gmx"),
            Cursor::new(xml),
        )
        .unwrap();

        assert_eq!(config.name, "Default");
        assert_eq!(config.options().len(), 3);
        assert_eq!(config.option("option_gameid"), Some("42"));
        assert_eq!(config.option("option_empty"), Some(""));
        assert_eq!(config.option("option_name"), Some("Rock & Roll"));
        assert_eq!(config.constant("LIMIT"), Some("100"));
    }

    #[test]
    fn supports_legacy_root_options() {
        let xml =
            b"<Config><option_gameid>7</option_gameid><option_name>old</option_name></Config>";
        let config = parse(
            "Legacy".to_owned(),
            Path::new("Legacy.config.gmx"),
            Cursor::new(xml),
        )
        .unwrap();

        assert_eq!(config.options().len(), 2);
        assert_eq!(config.option("option_gameid"), Some("7"));
    }

    #[test]
    fn rejects_duplicate_options() {
        let xml = b"<Config><Options><same>1</same><same>2</same></Options></Config>";
        let error = parse(
            "Bad".to_owned(),
            Path::new("Bad.config.gmx"),
            Cursor::new(xml),
        )
        .unwrap_err();

        assert!(matches!(error, ConfigError::DuplicateOption { .. }));
    }
}
