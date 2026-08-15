use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const FILE_HEADER_SIZE: u64 = 8;
const CHUNK_HEADER_SIZE: u64 = 8;

/// A four-byte identifier used by IFF/WAD files.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FourCc([u8; 4]);

impl FourCc {
    pub const FORM: Self = Self(*b"FORM");

    pub const fn new(bytes: [u8; 4]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }

    fn file_name(self) -> String {
        if self
            .0
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.0.iter().map(|byte| char::from(*byte)).collect()
        } else {
            self.0.iter().map(|byte| format!("{byte:02X}")).collect()
        }
    }
}

impl fmt::Display for FourCc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            if byte.is_ascii_graphic() || byte == b' ' {
                write!(formatter, "{}", char::from(byte))?;
            } else {
                write!(formatter, "\\x{byte:02X}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for FourCc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "FourCc(\"{self}\")")
    }
}

/// The location and declared payload size of one WAD chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub name: FourCc,
    pub header_offset: u64,
    pub data_offset: u64,
    pub size: u32,
}

impl Chunk {
    pub const fn end_offset(self) -> u64 {
        self.data_offset + self.size as u64
    }
}

/// The top-level structure of a parsed WAD file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WadFile {
    pub form_size: u32,
    pub file_size: u64,
    pub chunks: Vec<Chunk>,
}

impl WadFile {
    /// Reads only the WAD structure. Chunk payloads are skipped with seeks.
    pub fn read<R: Read + Seek>(reader: &mut R) -> Result<Self, ReadError> {
        let file_size = reader.seek(SeekFrom::End(0))?;
        if file_size < FILE_HEADER_SIZE {
            return Err(ReadError::FileTooSmall { file_size });
        }

        reader.seek(SeekFrom::Start(0))?;
        let magic = read_four_cc(reader)?;
        if magic != FourCc::FORM {
            return Err(ReadError::InvalidMagic { actual: magic });
        }

        let form_size = read_u32(reader)?;
        let form_end = FILE_HEADER_SIZE + u64::from(form_size);
        if form_end > file_size {
            return Err(ReadError::FormPastFile {
                form_end,
                file_size,
            });
        }

        let mut chunks = Vec::new();
        let mut offset = FILE_HEADER_SIZE;
        while offset < form_end {
            let remaining = form_end - offset;
            if remaining < CHUNK_HEADER_SIZE {
                return Err(ReadError::TruncatedChunkHeader { offset, remaining });
            }

            reader.seek(SeekFrom::Start(offset))?;
            let name = read_four_cc(reader)?;
            let size = read_u32(reader)?;
            let data_offset = offset + CHUNK_HEADER_SIZE;
            let chunk_end = data_offset + u64::from(size);
            if chunk_end > form_end {
                return Err(ReadError::ChunkPastForm {
                    name,
                    header_offset: offset,
                    chunk_end,
                    form_end,
                });
            }

            chunks.push(Chunk {
                name,
                header_offset: offset,
                data_offset,
                size,
            });
            offset = chunk_end;
        }

        Ok(Self {
            form_size,
            file_size,
            chunks,
        })
    }

    pub const fn form_end(&self) -> u64 {
        FILE_HEADER_SIZE + self.form_size as u64
    }

    pub const fn trailing_size(&self) -> u64 {
        self.file_size - self.form_end()
    }
}

trait WriteSeek: Write + Seek {}

impl<T: Write + Seek + ?Sized> WriteSeek for T {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ChunkGroup {
    Normal = 1,
    Texture = 2,
    Audio = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkOptions {
    pub group: ChunkGroup,
    pub alignment: u32,
    pub alignment_offset: u32,
}

impl ChunkOptions {
    pub const NORMAL: Self = Self {
        group: ChunkGroup::Normal,
        alignment: 16,
        alignment_offset: 0,
    };

    pub const TEXTURE: Self = Self {
        group: ChunkGroup::Texture,
        alignment: 128,
        alignment_offset: 0,
    };

    pub const AUDIO: Self = Self {
        group: ChunkGroup::Audio,
        alignment: 16,
        alignment_offset: 0,
    };
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self::NORMAL
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetPatch {
    offset: u64,
}

#[derive(Debug, Default)]
pub struct StringTable {
    inner: RefCell<StringTableInner>,
}

#[derive(Debug, Default)]
struct StringTableInner {
    entries: Vec<StringEntry>,
    index: HashMap<String, usize>,
    written: bool,
}

#[derive(Debug)]
struct StringEntry {
    value: String,
    references: Vec<OffsetPatch>,
}

impl StringTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.inner.borrow().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Interns a string in first-use order without emitting a reference.
    pub fn intern(&self, value: &str) -> Result<usize, WriteError> {
        let mut inner = self.inner.borrow_mut();
        if inner.written {
            return Err(WriteError::StringTableFinalized);
        }
        Ok(intern_string(&mut inner, value))
    }

    /// Writes a placeholder that is relocated to the UTF-8 bytes in STRG.
    /// GMS string references point after the record's byte-length field.
    pub fn write_reference(
        &self,
        writer: &mut ChunkWriter<'_>,
        value: &str,
    ) -> Result<(), WriteError> {
        let mut inner = self.inner.borrow_mut();
        if inner.written {
            return Err(WriteError::StringTableFinalized);
        }
        let patch = writer.reserve_u32()?;
        let index = intern_string(&mut inner, value);
        inner.entries[index].references.push(patch);
        Ok(())
    }

    /// Registers an already-written zero word for relocation to `value`.
    pub fn add_reference_at(&self, value: &str, offset: u64) -> Result<(), WriteError> {
        let mut inner = self.inner.borrow_mut();
        if inner.written {
            return Err(WriteError::StringTableFinalized);
        }
        let index = intern_string(&mut inner, value);
        inner.entries[index].references.push(OffsetPatch { offset });
        Ok(())
    }

    /// Serializes the complete STRG offset table and resolves every reference.
    /// This must run after every chunk that can add strings.
    pub fn write_strg(&self, writer: &mut ChunkWriter<'_>) -> Result<(), WriteError> {
        let mut inner = self.inner.borrow_mut();
        if inner.written {
            return Err(WriteError::StringTableFinalized);
        }
        inner.written = true;

        let count = u32::try_from(inner.entries.len()).map_err(|_| WriteError::SizeOverflow {
            field: "string count",
            size: inner.entries.len() as u64,
        })?;
        writer.write_u32(count)?;
        let mut record_patches = Vec::with_capacity(inner.entries.len());
        for _ in &inner.entries {
            record_patches.push(writer.reserve_u32()?);
        }

        for (index, record_patch) in record_patches.into_iter().enumerate() {
            writer.patch_position(record_patch)?;
            let entry = &mut inner.entries[index];
            let byte_len =
                u32::try_from(entry.value.len()).map_err(|_| WriteError::SizeOverflow {
                    field: "UTF-8 string length",
                    size: entry.value.len() as u64,
                })?;
            writer.write_u32(byte_len)?;
            let string_offset = writer.position_u32()?;
            for reference in entry.references.drain(..) {
                writer.patch_u32(reference, string_offset)?;
            }
            writer.write_all(entry.value.as_bytes())?;
            writer.write_all(&[0])?;
        }
        Ok(())
    }
}

fn intern_string(inner: &mut StringTableInner, value: &str) -> usize {
    if let Some(index) = inner.index.get(value) {
        return *index;
    }
    let index = inner.entries.len();
    inner.entries.push(StringEntry {
        value: value.to_owned(),
        references: Vec::new(),
    });
    inner.index.insert(value.to_owned(), index);
    index
}

pub struct ChunkWriter<'a> {
    output: &'a mut dyn WriteSeek,
}

impl ChunkWriter<'_> {
    pub fn position(&mut self) -> Result<u64, WriteError> {
        Ok(self.output.stream_position()?)
    }

    pub fn position_u32(&mut self) -> Result<u32, WriteError> {
        let position = self.position()?;
        u32::try_from(position).map_err(|_| WriteError::SizeOverflow {
            field: "absolute offset",
            size: position,
        })
    }

    pub fn write_u32(&mut self, value: u32) -> Result<(), WriteError> {
        self.output.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    pub fn write_i32(&mut self, value: i32) -> Result<(), WriteError> {
        self.output.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    pub fn write_u16(&mut self, value: u16) -> Result<(), WriteError> {
        self.output.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    pub fn write_i16(&mut self, value: i16) -> Result<(), WriteError> {
        self.output.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    pub fn write_u64(&mut self, value: u64) -> Result<(), WriteError> {
        self.output.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    pub fn write_i64(&mut self, value: i64) -> Result<(), WriteError> {
        self.output.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    pub fn write_f32(&mut self, value: f32) -> Result<(), WriteError> {
        self.output.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    pub fn write_f64(&mut self, value: f64) -> Result<(), WriteError> {
        self.output.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    pub fn write_bool(&mut self, value: bool) -> Result<(), WriteError> {
        self.write_i32(i32::from(value))
    }

    pub fn reserve_u32(&mut self) -> Result<OffsetPatch, WriteError> {
        let offset = self.position()?;
        self.write_u32(0)?;
        Ok(OffsetPatch { offset })
    }

    pub fn patch_u32(&mut self, patch: OffsetPatch, value: u32) -> Result<(), WriteError> {
        let current = self.position()?;
        self.output.seek(SeekFrom::Start(patch.offset))?;
        self.output.write_all(&value.to_le_bytes())?;
        self.output.seek(SeekFrom::Start(current))?;
        Ok(())
    }

    /// Patches an absolute word that was emitted by a previous chunk.
    pub fn patch_u32_at(&mut self, offset: u64, value: u32) -> Result<(), WriteError> {
        self.patch_u32(OffsetPatch { offset }, value)
    }

    pub fn patch_position(&mut self, patch: OffsetPatch) -> Result<(), WriteError> {
        let position = self.position_u32()?;
        self.patch_u32(patch, position)
    }

    pub fn align(&mut self, alignment: u32) -> Result<(), WriteError> {
        validate_alignment(alignment, 0)?;
        align_output(self.output, alignment, 0)
    }

    pub fn write_offset_table<T, F>(
        &mut self,
        items: &[T],
        mut write_item: F,
    ) -> Result<(), WriteError>
    where
        F: FnMut(&mut Self, &T) -> Result<(), WriteError>,
    {
        let count = u32::try_from(items.len()).map_err(|_| WriteError::SizeOverflow {
            field: "offset table item count",
            size: items.len() as u64,
        })?;
        self.write_u32(count)?;
        let table_start = self.position()?;
        for _ in items {
            self.write_u32(0)?;
        }
        for (index, item) in items.iter().enumerate() {
            let patch = OffsetPatch {
                offset: table_start + index as u64 * 4,
            };
            self.patch_position(patch)?;
            write_item(self, item)?;
        }
        Ok(())
    }
}

impl Write for ChunkWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.output.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

type ChunkSerializer<'a> =
    dyn for<'writer> Fn(&mut ChunkWriter<'writer>) -> Result<(), WriteError> + 'a;

struct PendingChunk<'a> {
    name: FourCc,
    options: ChunkOptions,
    sequence: usize,
    serialize: Box<ChunkSerializer<'a>>,
}

pub struct WadBuilder<'a> {
    chunks: Vec<PendingChunk<'a>>,
}

impl<'a> WadBuilder<'a> {
    pub const fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    pub fn add_chunk<F>(&mut self, name: FourCc, serialize: F) -> Result<(), WriteError>
    where
        F: for<'writer> Fn(&mut ChunkWriter<'writer>) -> Result<(), WriteError> + 'a,
    {
        self.add_chunk_with(name, ChunkOptions::NORMAL, serialize)
    }

    pub fn add_chunk_with<F>(
        &mut self,
        name: FourCc,
        options: ChunkOptions,
        serialize: F,
    ) -> Result<(), WriteError>
    where
        F: for<'writer> Fn(&mut ChunkWriter<'writer>) -> Result<(), WriteError> + 'a,
    {
        validate_alignment(options.alignment, options.alignment_offset)?;
        if self.chunks.iter().any(|chunk| chunk.name == name) {
            return Err(WriteError::DuplicateChunk { name });
        }
        self.chunks.push(PendingChunk {
            name,
            options,
            sequence: self.chunks.len(),
            serialize: Box::new(serialize),
        });
        Ok(())
    }

    pub fn write_to<W>(mut self, output: &mut W) -> Result<WadFile, WriteError>
    where
        W: Write + Seek,
    {
        let start = output.stream_position()?;
        if start != 0 {
            return Err(WriteError::NonZeroStart { position: start });
        }

        self.chunks
            .sort_by_key(|chunk| (chunk.options.group, chunk.sequence));
        write_four_cc(output, FourCc::FORM)?;
        let form_size_patch = reserve_u32(output)?;

        let mut previous_size_patch = None;
        let mut written = Vec::<Chunk>::with_capacity(self.chunks.len());
        for chunk in self.chunks {
            if let Some(size_patch) = previous_size_patch.take() {
                align_output(
                    output,
                    chunk.options.alignment,
                    chunk.options.alignment_offset,
                )?;
                let size = patch_size(output, size_patch, "chunk size")?;
                written.last_mut().unwrap().size = size;
            }

            let header_offset = output.stream_position()?;
            write_four_cc(output, chunk.name)?;
            let size_patch = reserve_u32(output)?;
            let data_offset = output.stream_position()?;
            (chunk.serialize)(&mut ChunkWriter { output }).map_err(|source| WriteError::Chunk {
                name: chunk.name,
                source: Box::new(source),
            })?;
            written.push(Chunk {
                name: chunk.name,
                header_offset,
                data_offset,
                size: 0,
            });
            previous_size_patch = Some(size_patch);
        }

        if let Some(size_patch) = previous_size_patch {
            let size = patch_size(output, size_patch, "chunk size")?;
            written.last_mut().unwrap().size = size;
        }
        let form_size = patch_size(output, form_size_patch, "FORM size")?;
        let file_size = output.stream_position()?;
        output.flush()?;

        Ok(WadFile {
            form_size,
            file_size,
            chunks: written,
        })
    }
}

impl Default for WadBuilder<'_> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum WriteError {
    Io(io::Error),
    Chunk { name: FourCc, source: Box<Self> },
    NonZeroStart { position: u64 },
    DuplicateChunk { name: FourCc },
    InvalidAlignment { alignment: u32, offset: u32 },
    SizeOverflow { field: &'static str, size: u64 },
    StringTableFinalized,
    InvalidVmData { message: String },
}

impl fmt::Display for WriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Chunk { name, source } => write!(formatter, "WAD chunk {name}: {source}"),
            Self::NonZeroStart { position } => {
                write!(
                    formatter,
                    "WAD output starts at offset {position}; expected 0"
                )
            }
            Self::DuplicateChunk { name } => write!(formatter, "duplicate WAD chunk {name}"),
            Self::InvalidAlignment { alignment, offset } => write!(
                formatter,
                "invalid chunk alignment {alignment} with offset {offset}; alignment must be a power of two and offset must be smaller"
            ),
            Self::SizeOverflow { field, size } => {
                write!(
                    formatter,
                    "{field} value {size} does not fit in a 32-bit WAD field"
                )
            }
            Self::StringTableFinalized => {
                formatter.write_str("STRG was already finalized; no more strings can be added")
            }
            Self::InvalidVmData { message } => write!(formatter, "invalid VM data: {message}"),
        }
    }
}

impl Error for WriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Chunk { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for WriteError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn validate_alignment(alignment: u32, offset: u32) -> Result<(), WriteError> {
    if !alignment.is_power_of_two() || offset >= alignment {
        return Err(WriteError::InvalidAlignment { alignment, offset });
    }
    Ok(())
}

fn align_output(output: &mut dyn WriteSeek, alignment: u32, offset: u32) -> Result<(), WriteError> {
    let position = output.stream_position()?;
    let remainder = position % u64::from(alignment);
    let padding = (u64::from(offset) + u64::from(alignment) - remainder) % u64::from(alignment);
    write_zeros(output, padding)?;
    Ok(())
}

fn write_zeros(output: &mut dyn Write, mut count: u64) -> io::Result<()> {
    const ZEROS: [u8; 128] = [0; 128];
    while count != 0 {
        let amount = count.min(ZEROS.len() as u64) as usize;
        output.write_all(&ZEROS[..amount])?;
        count -= amount as u64;
    }
    Ok(())
}

fn write_four_cc(output: &mut dyn Write, value: FourCc) -> io::Result<()> {
    output.write_all(value.as_bytes())
}

fn reserve_u32(output: &mut dyn WriteSeek) -> Result<OffsetPatch, WriteError> {
    let offset = output.stream_position()?;
    output.write_all(&0_u32.to_le_bytes())?;
    Ok(OffsetPatch { offset })
}

fn patch_size(
    output: &mut dyn WriteSeek,
    patch: OffsetPatch,
    field: &'static str,
) -> Result<u32, WriteError> {
    let end = output.stream_position()?;
    let size = end - patch.offset - 4;
    let size = u32::try_from(size).map_err(|_| WriteError::SizeOverflow { field, size })?;
    output.seek(SeekFrom::Start(patch.offset))?;
    output.write_all(&size.to_le_bytes())?;
    output.seek(SeekFrom::Start(end))?;
    Ok(size)
}

/// Copies every chunk payload into a separate file without loading it in memory.
/// Existing files are never overwritten.
pub fn extract_chunks<R: Read + Seek>(
    reader: &mut R,
    wad: &WadFile,
    output_dir: &Path,
) -> io::Result<Vec<PathBuf>> {
    fs::create_dir_all(output_dir)?;

    let width = wad.chunks.len().saturating_sub(1).to_string().len().max(2);
    let mut paths = Vec::with_capacity(wad.chunks.len());

    for (index, chunk) in wad.chunks.iter().enumerate() {
        let file_name = format!(
            "{index:0width$}_{}.bin",
            chunk.name.file_name(),
            width = width
        );
        let path = output_dir.join(file_name);
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut output = BufWriter::new(output);

        reader.seek(SeekFrom::Start(chunk.data_offset))?;
        let mut payload = reader.by_ref().take(u64::from(chunk.size));
        let copied = io::copy(&mut payload, &mut output)?;
        if copied != u64::from(chunk.size) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "chunk {} ended after {copied} of {} bytes",
                    chunk.name, chunk.size
                ),
            ));
        }
        output.flush()?;
        paths.push(path);
    }

    Ok(paths)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueDifference<T> {
    pub expected: T,
    pub actual: T,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadDifference {
    /// Offset relative to the start of the chunk payload.
    pub first_offset: u64,
    /// Number of unequal bytes, including bytes present on only one side.
    pub differing_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDifference {
    pub name: FourCc,
    /// Distinguishes duplicate chunk names, starting at zero.
    pub occurrence: usize,
    pub expected_index: Option<usize>,
    pub actual_index: Option<usize>,
    pub header_offset: Option<ValueDifference<u64>>,
    pub data_offset: Option<ValueDifference<u64>>,
    pub size: Option<ValueDifference<u32>>,
    pub payload: Option<PayloadDifference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WadDiff {
    pub form_size: Option<ValueDifference<u32>>,
    pub file_size: Option<ValueDifference<u64>>,
    pub trailing_size: Option<ValueDifference<u64>>,
    pub chunk_order: Option<ValueDifference<Vec<FourCc>>>,
    pub chunks: Vec<ChunkDifference>,
}

impl WadDiff {
    pub fn is_identical(&self) -> bool {
        self.form_size.is_none()
            && self.file_size.is_none()
            && self.trailing_size.is_none()
            && self.chunk_order.is_none()
            && self.chunks.is_empty()
    }
}

/// Compares WAD structure field-by-field and streams chunk payload comparison
/// in fixed-size buffers, so even large texture/audio chunks do not get copied
/// into memory.
pub fn diff_wads<E: Read + Seek, A: Read + Seek>(
    expected_reader: &mut E,
    actual_reader: &mut A,
) -> Result<WadDiff, DiffError> {
    let expected = WadFile::read(expected_reader).map_err(DiffError::Expected)?;
    let actual = WadFile::read(actual_reader).map_err(DiffError::Actual)?;
    let expected_order = expected
        .chunks
        .iter()
        .map(|chunk| chunk.name)
        .collect::<Vec<_>>();
    let actual_order = actual
        .chunks
        .iter()
        .map(|chunk| chunk.name)
        .collect::<Vec<_>>();

    let mut actual_occurrences = HashMap::<FourCc, usize>::new();
    let mut actual_by_key = HashMap::<(FourCc, usize), usize>::new();
    for (index, chunk) in actual.chunks.iter().enumerate() {
        let occurrence = actual_occurrences.entry(chunk.name).or_default();
        actual_by_key.insert((chunk.name, *occurrence), index);
        *occurrence += 1;
    }

    let mut chunks = Vec::new();
    let mut expected_occurrences = HashMap::<FourCc, usize>::new();
    for (expected_index, expected_chunk) in expected.chunks.iter().enumerate() {
        let occurrence = expected_occurrences.entry(expected_chunk.name).or_default();
        let key = (expected_chunk.name, *occurrence);
        *occurrence += 1;
        let Some(actual_index) = actual_by_key.remove(&key) else {
            chunks.push(ChunkDifference {
                name: expected_chunk.name,
                occurrence: key.1,
                expected_index: Some(expected_index),
                actual_index: None,
                header_offset: None,
                data_offset: None,
                size: None,
                payload: None,
            });
            continue;
        };
        let actual_chunk = &actual.chunks[actual_index];
        let payload =
            compare_payloads(expected_reader, expected_chunk, actual_reader, actual_chunk)?;
        let difference = ChunkDifference {
            name: expected_chunk.name,
            occurrence: key.1,
            expected_index: Some(expected_index),
            actual_index: Some(actual_index),
            header_offset: value_difference(
                expected_chunk.header_offset,
                actual_chunk.header_offset,
            ),
            data_offset: value_difference(expected_chunk.data_offset, actual_chunk.data_offset),
            size: value_difference(expected_chunk.size, actual_chunk.size),
            payload,
        };
        if difference.header_offset.is_some()
            || difference.data_offset.is_some()
            || difference.size.is_some()
            || difference.payload.is_some()
            || expected_index != actual_index
        {
            chunks.push(difference);
        }
    }
    let mut unexpected = actual_by_key.into_iter().collect::<Vec<_>>();
    unexpected.sort_by_key(|(_, index)| *index);
    for ((name, occurrence), actual_index) in unexpected {
        chunks.push(ChunkDifference {
            name,
            occurrence,
            expected_index: None,
            actual_index: Some(actual_index),
            header_offset: None,
            data_offset: None,
            size: None,
            payload: None,
        });
    }

    Ok(WadDiff {
        form_size: value_difference(expected.form_size, actual.form_size),
        file_size: value_difference(expected.file_size, actual.file_size),
        trailing_size: value_difference(expected.trailing_size(), actual.trailing_size()),
        chunk_order: value_difference(expected_order, actual_order),
        chunks,
    })
}

fn value_difference<T: PartialEq>(expected: T, actual: T) -> Option<ValueDifference<T>> {
    (expected != actual).then_some(ValueDifference { expected, actual })
}

fn compare_payloads<E: Read + Seek, A: Read + Seek>(
    expected_reader: &mut E,
    expected: &Chunk,
    actual_reader: &mut A,
    actual: &Chunk,
) -> Result<Option<PayloadDifference>, DiffError> {
    const BUFFER_SIZE: usize = 16 * 1024;
    expected_reader.seek(SeekFrom::Start(expected.data_offset))?;
    actual_reader.seek(SeekFrom::Start(actual.data_offset))?;
    let common_size = u64::from(expected.size.min(actual.size));
    let mut expected_buffer = [0; BUFFER_SIZE];
    let mut actual_buffer = [0; BUFFER_SIZE];
    let mut offset = 0_u64;
    let mut first_offset = None;
    let mut differing_bytes = 0_u64;
    while offset < common_size {
        let length = usize::try_from((common_size - offset).min(BUFFER_SIZE as u64)).unwrap();
        expected_reader.read_exact(&mut expected_buffer[..length])?;
        actual_reader.read_exact(&mut actual_buffer[..length])?;
        for index in 0..length {
            if expected_buffer[index] != actual_buffer[index] {
                first_offset.get_or_insert(offset + index as u64);
                differing_bytes += 1;
            }
        }
        offset += length as u64;
    }
    let extra = u64::from(expected.size.abs_diff(actual.size));
    if extra != 0 {
        first_offset.get_or_insert(common_size);
        differing_bytes += extra;
    }
    Ok(first_offset.map(|first_offset| PayloadDifference {
        first_offset,
        differing_bytes,
    }))
}

#[derive(Debug)]
pub enum DiffError {
    Expected(ReadError),
    Actual(ReadError),
    Io(io::Error),
}

impl fmt::Display for DiffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expected(error) => write!(formatter, "cannot read expected WAD: {error}"),
            Self::Actual(error) => write!(formatter, "cannot read actual WAD: {error}"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl Error for DiffError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Expected(error) | Self::Actual(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for DiffError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    FileTooSmall {
        file_size: u64,
    },
    InvalidMagic {
        actual: FourCc,
    },
    FormPastFile {
        form_end: u64,
        file_size: u64,
    },
    TruncatedChunkHeader {
        offset: u64,
        remaining: u64,
    },
    ChunkPastForm {
        name: FourCc,
        header_offset: u64,
        chunk_end: u64,
        form_end: u64,
    },
}

impl fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::FileTooSmall { file_size } => {
                write!(
                    formatter,
                    "file is only {file_size} bytes; expected a FORM header"
                )
            }
            Self::InvalidMagic { actual } => {
                write!(formatter, "invalid magic {actual}; expected FORM")
            }
            Self::FormPastFile {
                form_end,
                file_size,
            } => write!(
                formatter,
                "FORM ends at offset {form_end}, past the {file_size}-byte file"
            ),
            Self::TruncatedChunkHeader { offset, remaining } => write!(
                formatter,
                "only {remaining} bytes remain for the chunk header at offset {offset}"
            ),
            Self::ChunkPastForm {
                name,
                header_offset,
                chunk_end,
                form_end,
            } => write!(
                formatter,
                "chunk {name} at offset {header_offset} ends at {chunk_end}, past FORM end {form_end}"
            ),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn read_four_cc(reader: &mut impl Read) -> io::Result<FourCc> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(FourCc::new(bytes))
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        ChunkGroup, ChunkOptions, FourCc, ReadError, StringTable, WadBuilder, WadFile, WriteError,
        diff_wads, extract_chunks,
    };

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!("gmx-rs-test-{}-{nonce}", std::process::id())))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            if self.0.starts_with(std::env::temp_dir()) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
    }

    fn make_wad(chunks: &[([u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (name, payload) in chunks {
            body.extend_from_slice(name);
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
        }

        let mut file = Vec::new();
        file.extend_from_slice(b"FORM");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);
        file
    }

    #[test]
    fn writes_sorted_chunks_with_official_alignment_and_backpatching() {
        let mut builder = WadBuilder::new();
        builder
            .add_chunk_with(FourCc::new(*b"AUDO"), ChunkOptions::AUDIO, |writer| {
                writer.write_all(b"audio")?;
                Ok(())
            })
            .unwrap();
        builder
            .add_chunk(FourCc::new(*b"GEN8"), |writer| {
                writer.write_all(b"abc")?;
                Ok(())
            })
            .unwrap();
        builder
            .add_chunk_with(FourCc::new(*b"TXTR"), ChunkOptions::TEXTURE, |writer| {
                writer.write_all(&[0x77])?;
                Ok(())
            })
            .unwrap();
        builder
            .add_chunk(FourCc::new(*b"OPTN"), |writer| {
                writer.write_all(b"hello")?;
                Ok(())
            })
            .unwrap();

        let mut output = Cursor::new(Vec::new());
        let written = builder.write_to(&mut output).unwrap();
        let bytes = output.into_inner();
        let parsed = WadFile::read(&mut Cursor::new(&bytes)).unwrap();

        assert_eq!(written, parsed);
        assert_eq!(written.form_size, 149);
        assert_eq!(written.file_size, 157);
        assert_eq!(
            written
                .chunks
                .iter()
                .map(|chunk| chunk.name)
                .collect::<Vec<_>>(),
            [
                FourCc::new(*b"GEN8"),
                FourCc::new(*b"OPTN"),
                FourCc::new(*b"TXTR"),
                FourCc::new(*b"AUDO"),
            ]
        );
        assert_eq!(
            (written.chunks[0].header_offset, written.chunks[0].size),
            (8, 16)
        );
        assert_eq!(
            (written.chunks[1].header_offset, written.chunks[1].size),
            (32, 88)
        );
        assert_eq!(
            (written.chunks[2].header_offset, written.chunks[2].size),
            (128, 8)
        );
        assert_eq!(
            (written.chunks[3].header_offset, written.chunks[3].size),
            (144, 5)
        );
        assert!(bytes[19..32].iter().all(|byte| *byte == 0));
        assert!(bytes[45..128].iter().all(|byte| *byte == 0));
        assert!(bytes[137..144].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn writes_absolute_count_and_offset_tables() {
        let mut builder = WadBuilder::new();
        builder
            .add_chunk(FourCc::new(*b"TEST"), |writer| {
                writer.write_offset_table(&[0x1122_3344, 0x5566_7788], |writer, value| {
                    writer.write_u32(*value)
                })
            })
            .unwrap();
        let mut output = Cursor::new(Vec::new());
        builder.write_to(&mut output).unwrap();
        let bytes = output.into_inner();
        let value =
            |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());

        assert_eq!(value(16), 2);
        assert_eq!(value(20), 28);
        assert_eq!(value(24), 32);
        assert_eq!(value(28), 0x1122_3344);
        assert_eq!(value(32), 0x5566_7788);
    }

    #[test]
    fn interns_utf8_strings_and_relocates_references_to_their_bytes() {
        let strings = StringTable::new();
        let mut builder = WadBuilder::new();
        builder
            .add_chunk(FourCc::new(*b"TEST"), |writer| {
                strings.write_reference(writer, "hé")?;
                strings.write_reference(writer, "hé")?;
                strings.write_reference(writer, "other")
            })
            .unwrap();
        builder
            .add_chunk(FourCc::new(*b"STRG"), |writer| strings.write_strg(writer))
            .unwrap();
        let mut output = Cursor::new(Vec::new());
        let wad = builder.write_to(&mut output).unwrap();
        let bytes = output.into_inner();
        let u32_at =
            |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());

        assert_eq!(strings.len(), 2);
        assert_eq!(u32_at(16), u32_at(20));
        let first_record = u32_at(wad.chunks[1].data_offset as usize + 4) as usize;
        assert_eq!(u32_at(16) as usize, first_record + 4);
        assert_eq!(u32_at(first_record), "hé".len() as u32);
        assert_eq!(
            &bytes[first_record + 4..first_record + 4 + "hé".len()],
            "hé".as_bytes()
        );
        assert_eq!(bytes[first_record + 4 + "hé".len()], 0);
        assert!(matches!(
            strings.intern("late"),
            Err(WriteError::StringTableFinalized)
        ));
    }

    #[test]
    fn rejects_invalid_registration_and_nonzero_output_start() {
        let mut builder = WadBuilder::new();
        let invalid = ChunkOptions {
            group: ChunkGroup::Normal,
            alignment: 3,
            alignment_offset: 0,
        };
        assert!(matches!(
            builder.add_chunk_with(FourCc::new(*b"BAD!"), invalid, |_| Ok(())),
            Err(WriteError::InvalidAlignment { .. })
        ));

        builder
            .add_chunk(FourCc::new(*b"TEST"), |_| Ok(()))
            .unwrap();
        assert!(matches!(
            builder.add_chunk(FourCc::new(*b"TEST"), |_| Ok(())),
            Err(WriteError::DuplicateChunk { .. })
        ));

        let mut output = Cursor::new(vec![0]);
        output.set_position(1);
        assert!(matches!(
            builder.write_to(&mut output),
            Err(WriteError::NonZeroStart { position: 1 })
        ));
    }

    #[test]
    fn reports_the_chunk_that_failed_to_serialize() {
        let mut builder = WadBuilder::new();
        builder
            .add_chunk(FourCc::new(*b"SPRT"), |_| {
                Err(WriteError::InvalidVmData {
                    message: "sprite resource is invalid".to_owned(),
                })
            })
            .unwrap();
        let error = builder.write_to(&mut Cursor::new(Vec::new())).unwrap_err();

        assert!(matches!(
            &error,
            WriteError::Chunk {
                name,
                source: _
            } if *name == FourCc::new(*b"SPRT")
        ));
        assert_eq!(
            error.to_string(),
            "WAD chunk SPRT: invalid VM data: sprite resource is invalid"
        );
    }

    #[test]
    fn reads_chunk_table() {
        let bytes = make_wad(&[(*b"GEN8", b"abc"), (*b"STRG", b"hello")]);
        let wad = WadFile::read(&mut Cursor::new(bytes)).unwrap();

        assert_eq!(wad.file_size, 32);
        assert_eq!(wad.form_size, 24);
        assert_eq!(wad.trailing_size(), 0);
        assert_eq!(wad.chunks.len(), 2);
        assert_eq!(wad.chunks[0].name, FourCc::new(*b"GEN8"));
        assert_eq!(wad.chunks[0].header_offset, 8);
        assert_eq!(wad.chunks[0].data_offset, 16);
        assert_eq!(wad.chunks[0].size, 3);
        assert_eq!(wad.chunks[1].header_offset, 19);
        assert_eq!(wad.chunks[1].end_offset(), 32);
    }

    #[test]
    fn reports_invalid_magic() {
        let mut bytes = make_wad(&[]);
        bytes[..4].copy_from_slice(b"RIFF");

        let error = WadFile::read(&mut Cursor::new(bytes)).unwrap_err();
        assert!(matches!(error, ReadError::InvalidMagic { .. }));
    }

    #[test]
    fn reports_chunk_outside_form() {
        let mut bytes = make_wad(&[(*b"GEN8", b"abc")]);
        bytes[12..16].copy_from_slice(&10_u32.to_le_bytes());

        let error = WadFile::read(&mut Cursor::new(bytes)).unwrap_err();
        assert!(matches!(error, ReadError::ChunkPastForm { .. }));
    }

    #[test]
    fn allows_bytes_after_form() {
        let mut bytes = make_wad(&[(*b"GEN8", b"abc")]);
        bytes.extend_from_slice(b"trailing");

        let wad = WadFile::read(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(wad.trailing_size(), 8);
        assert_eq!(wad.chunks.len(), 1);
    }

    #[test]
    fn reports_structural_and_streamed_payload_differences() {
        let expected = make_wad(&[(*b"GEN8", b"abc"), (*b"STRG", b"hello")]);
        let actual = make_wad(&[(*b"GEN8", b"abd"), (*b"LANG", b"hello")]);
        let diff = diff_wads(&mut Cursor::new(expected), &mut Cursor::new(actual)).unwrap();

        assert!(diff.form_size.is_none());
        assert!(diff.file_size.is_none());
        assert!(diff.chunk_order.is_some());
        let gen8 = diff
            .chunks
            .iter()
            .find(|chunk| chunk.name == FourCc::new(*b"GEN8"))
            .unwrap();
        assert_eq!(gen8.payload.unwrap().first_offset, 2);
        assert_eq!(gen8.payload.unwrap().differing_bytes, 1);
        assert!(
            diff.chunks.iter().any(|chunk| {
                chunk.name == FourCc::new(*b"STRG") && chunk.actual_index.is_none()
            })
        );
        assert!(diff.chunks.iter().any(|chunk| {
            chunk.name == FourCc::new(*b"LANG") && chunk.expected_index.is_none()
        }));
    }

    #[test]
    fn extracts_payloads_without_overwriting() {
        let bytes = make_wad(&[(*b"GEN8", b"abc"), (*b"STRG", b"hello")]);
        let mut reader = Cursor::new(bytes);
        let wad = WadFile::read(&mut reader).unwrap();
        let output = TestDir::new();

        let paths = extract_chunks(&mut reader, &wad, output.path()).unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(fs::read(output.path().join("00_GEN8.bin")).unwrap(), b"abc");
        assert_eq!(
            fs::read(output.path().join("01_STRG.bin")).unwrap(),
            b"hello"
        );

        let error = extract_chunks(&mut reader, &wad, output.path()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }
}
