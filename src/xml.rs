use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use quick_xml::encoding::Decoder;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    pub name: String,
    pub text: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|child| child.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Self> {
        self.children.iter().filter(move |child| child.name == name)
    }

    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug)]
pub(crate) enum FileError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Xml {
        path: PathBuf,
        offset: u64,
        message: String,
    },
    MissingRoot {
        path: PathBuf,
    },
    MultipleRoots {
        path: PathBuf,
    },
}

impl fmt::Display for FileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot read XML {}: {source}", path.display())
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
            Self::MissingRoot { path } => {
                write!(formatter, "XML file {} has no root element", path.display())
            }
            Self::MultipleRoots { path } => write!(
                formatter,
                "XML file {} contains multiple root elements",
                path.display()
            ),
        }
    }
}

impl Error for FileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn load(path: &Path) -> Result<Node, FileError> {
    let file = File::open(path).map_err(|source| FileError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(path, BufReader::new(file))
}

pub(crate) fn parse(path: &Path, input: impl BufRead) -> Result<Node, FileError> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().expand_empty_elements = true;

    let mut stack: Vec<Node> = Vec::new();
    let mut root = None;
    let mut buffer = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| file_xml_error(path, reader.buffer_position(), error))?;
        match event {
            Event::Start(start) => {
                let name = reader
                    .decoder()
                    .decode(start.name().as_ref())
                    .map_err(|error| file_xml_error(path, reader.buffer_position(), error))?
                    .into_owned();
                let attributes = all_attributes(&start, reader.decoder())
                    .map_err(|error| file_xml_error(path, reader.buffer_position(), error))?;
                stack.push(Node {
                    name,
                    text: String::new(),
                    attributes,
                    children: Vec::new(),
                });
            }
            Event::End(end) => {
                let end_name = reader
                    .decoder()
                    .decode(end.name().as_ref())
                    .map_err(|error| file_xml_error(path, reader.buffer_position(), error))?
                    .into_owned();
                let node = stack.pop().ok_or_else(|| {
                    file_xml_error(
                        path,
                        reader.buffer_position(),
                        "closing tag without an opening tag",
                    )
                })?;
                if node.name != end_name {
                    return Err(file_xml_error(
                        path,
                        reader.buffer_position(),
                        format!("closing tag {end_name:?} does not match {:?}", node.name),
                    ));
                }
                if let Some(parent) = stack.last_mut() {
                    parent.text.push_str(&node.text);
                    parent.children.push(node);
                } else if root.replace(node).is_some() {
                    return Err(FileError::MultipleRoots {
                        path: path.to_path_buf(),
                    });
                }
            }
            Event::Text(text) => {
                let text = text
                    .xml10_content()
                    .map_err(|error| file_xml_error(path, reader.buffer_position(), error))?;
                append_node_text(&mut stack, &text);
            }
            Event::CData(text) => {
                let text = text
                    .decode()
                    .map_err(|error| file_xml_error(path, reader.buffer_position(), error))?;
                append_node_text(&mut stack, &text);
            }
            Event::GeneralRef(reference) => {
                let text = resolve_reference(&reference)
                    .map_err(|error| file_xml_error(path, reader.buffer_position(), error))?;
                append_node_text(&mut stack, &text);
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

    if !stack.is_empty() {
        return Err(file_xml_error(path, 0, "XML ended with unclosed elements"));
    }
    root.ok_or_else(|| FileError::MissingRoot {
        path: path.to_path_buf(),
    })
}

pub(crate) fn attribute(
    element: &BytesStart<'_>,
    name: &[u8],
    decoder: Decoder,
) -> Result<Option<String>, String> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        if attribute.key.as_ref() == name {
            return attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
                .map(|value| Some(value.into_owned()))
                .map_err(|error| error.to_string());
        }
    }
    Ok(None)
}

fn all_attributes(
    element: &BytesStart<'_>,
    decoder: Decoder,
) -> Result<Vec<(String, String)>, String> {
    let mut values = Vec::new();
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        let name = decoder
            .decode(attribute.key.as_ref())
            .map_err(|error| error.to_string())?
            .into_owned();
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
            .map_err(|error| error.to_string())?
            .into_owned();
        values.push((name, value));
    }
    Ok(values)
}

pub(crate) fn resolve_reference(reference: &BytesRef<'_>) -> Result<String, String> {
    if let Some(character) = reference
        .resolve_char_ref()
        .map_err(|error| error.to_string())?
    {
        return Ok(character.to_string());
    }

    let name = reference.decode().map_err(|error| error.to_string())?;
    match name.as_ref() {
        "amp" => Ok("&".to_owned()),
        "lt" => Ok("<".to_owned()),
        "gt" => Ok(">".to_owned()),
        "apos" => Ok("'".to_owned()),
        "quot" => Ok("\"".to_owned()),
        _ => Err(format!("unsupported entity reference &{name};")),
    }
}

fn append_node_text(stack: &mut [Node], text: &str) {
    if let Some(node) = stack.last_mut() {
        node.text.push_str(text);
    }
}

fn file_xml_error(path: &Path, offset: u64, error: impl fmt::Display) -> FileError {
    FileError::Xml {
        path: path.to_path_buf(),
        offset,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use super::parse;

    #[test]
    fn builds_minimal_tree_with_inner_text() {
        let xml = b"<root flag=\"yes\">before<items><item>A&amp;B</item></items>after</root>";
        let root = parse(Path::new("test.xml"), Cursor::new(xml)).unwrap();

        assert_eq!(root.name, "root");
        assert_eq!(root.attributes, [("flag".to_owned(), "yes".to_owned())]);
        assert_eq!(root.text, "beforeA&Bafter");
        assert_eq!(
            root.child("items").unwrap().child("item").unwrap().text,
            "A&B"
        );
    }
}
