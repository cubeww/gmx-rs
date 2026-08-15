use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::io::{self, Cursor, Read, Seek, SeekFrom};

use image::ImageError;

use crate::wad::{Chunk, FourCc, ReadError, WadFile};

const GEN8: FourCc = FourCc::new(*b"GEN8");
const CODE: FourCc = FourCc::new(*b"CODE");
const VARI: FourCc = FourCc::new(*b"VARI");
const FUNC: FourCc = FourCc::new(*b"FUNC");
const STRG: FourCc = FourCc::new(*b"STRG");
const TXTR: FourCc = FourCc::new(*b"TXTR");
const AUDO: FourCc = FourCc::new(*b"AUDO");

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticWadDiff {
    pub differences: Vec<String>,
}

impl SemanticWadDiff {
    pub fn is_equivalent(&self) -> bool {
        self.differences.is_empty()
    }
}

pub fn diff_wads_semantic<E: Read + Seek, A: Read + Seek>(
    expected: &mut E,
    actual: &mut A,
) -> Result<SemanticWadDiff, SemanticDiffError> {
    let expected = ParsedWad::read(expected, "expected")?;
    let actual = ParsedWad::read(actual, "actual")?;
    compare_wads(&expected, &actual)
}

struct ParsedWad {
    label: &'static str,
    bytes: Vec<u8>,
    wad: WadFile,
}

impl ParsedWad {
    fn read(
        reader: &mut (impl Read + Seek),
        label: &'static str,
    ) -> Result<Self, SemanticDiffError> {
        reader.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        let wad = WadFile::read(&mut Cursor::new(&bytes))
            .map_err(|source| SemanticDiffError::InvalidWad { label, source })?;
        Ok(Self { label, bytes, wad })
    }

    fn chunk(&self, name: FourCc) -> Option<&Chunk> {
        self.wad.chunks.iter().find(|chunk| chunk.name == name)
    }

    fn payload(&self, chunk: &Chunk) -> &[u8] {
        &self.bytes[chunk.data_offset as usize..chunk.end_offset() as usize]
    }

    fn malformed(&self, message: impl Into<String>) -> SemanticDiffError {
        SemanticDiffError::Malformed {
            label: self.label,
            message: message.into(),
        }
    }

    fn read_u16(&self, offset: u64, field: &str) -> Result<u16, SemanticDiffError> {
        let bytes = self.range(offset, 2, field)?;
        Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_u32(&self, offset: u64, field: &str) -> Result<u32, SemanticDiffError> {
        let bytes = self.range(offset, 4, field)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_i32(&self, offset: u64, field: &str) -> Result<i32, SemanticDiffError> {
        Ok(self.read_u32(offset, field)? as i32)
    }

    fn range(&self, offset: u64, size: u64, field: &str) -> Result<&[u8], SemanticDiffError> {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| self.malformed(format!("{field} range overflows")))?;
        let range = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(end).ok())
            .filter(|(_, end)| *end <= self.bytes.len())
            .ok_or_else(|| {
                self.malformed(format!(
                    "{field} range {offset:#x}..{end:#x} is outside the file"
                ))
            })?;
        Ok(&self.bytes[range.0..range.1])
    }

    fn string(&self, pointer: u32, field: &str) -> Result<String, SemanticDiffError> {
        let pointer = u64::from(pointer);
        let length_offset = pointer
            .checked_sub(4)
            .ok_or_else(|| self.malformed(format!("{field} has null string pointer")))?;
        let length = u64::from(self.read_u32(length_offset, field)?);
        let bytes = self.range(pointer, length, field)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|source| self.malformed(format!("{field} is not UTF-8: {source}")))
    }
}

fn compare_wads(
    expected: &ParsedWad,
    actual: &ParsedWad,
) -> Result<SemanticWadDiff, SemanticDiffError> {
    let mut differences = Vec::new();
    let expected_order = expected
        .wad
        .chunks
        .iter()
        .map(|chunk| chunk.name)
        .collect::<Vec<_>>();
    let actual_order = actual
        .wad
        .chunks
        .iter()
        .map(|chunk| chunk.name)
        .collect::<Vec<_>>();
    if expected_order != actual_order {
        differences.push(format!(
            "chunk order differs: expected {}, actual {}",
            four_cc_list(&expected_order),
            four_cc_list(&actual_order)
        ));
    }

    let mut actual_chunks = HashMap::new();
    for chunk in &actual.wad.chunks {
        if actual_chunks.insert(chunk.name, chunk).is_some() {
            return Err(actual.malformed(format!(
                "duplicate {} chunks are not supported by semantic diff",
                chunk.name
            )));
        }
    }
    for chunk in &expected.wad.chunks {
        let Some(other) = actual_chunks.remove(&chunk.name) else {
            differences.push(format!("chunk {} is missing", chunk.name));
            continue;
        };
        if matches!(chunk.name, CODE | VARI | FUNC | STRG) {
            continue;
        }
        let left = expected.payload(chunk);
        let right = actual.payload(other);
        if left == right {
            continue;
        }
        match chunk.name {
            GEN8 => compare_gen8(expected, chunk, actual, other, &mut differences)?,
            TXTR => compare_textures(expected, chunk, actual, other, &mut differences)?,
            AUDO => compare_audio(expected, chunk, actual, other, &mut differences)?,
            name => differences.push(payload_difference(name, left, right)),
        }
    }
    for chunk in actual_chunks.values() {
        differences.push(format!("chunk {} is unexpected", chunk.name));
    }

    let expected_vm = [CODE, VARI, FUNC, STRG]
        .into_iter()
        .all(|name| expected.chunk(name).is_some());
    let actual_vm = [CODE, VARI, FUNC, STRG]
        .into_iter()
        .all(|name| actual.chunk(name).is_some());
    if expected_vm && actual_vm {
        compare_vm(expected, actual, &mut differences)?;
    } else if expected_vm != actual_vm {
        differences.push("CODE/VARI/FUNC/STRG chunk set differs".to_owned());
    }

    Ok(SemanticWadDiff { differences })
}

fn compare_gen8(
    expected: &ParsedWad,
    expected_chunk: &Chunk,
    actual: &ParsedWad,
    actual_chunk: &Chunk,
    differences: &mut Vec<String>,
) -> Result<(), SemanticDiffError> {
    let mut left = expected.payload(expected_chunk).to_vec();
    let mut right = actual.payload(actual_chunk).to_vec();
    if left.len() < 104 || right.len() < 104 {
        differences.push(payload_difference(GEN8, &left, &right));
        return Ok(());
    }
    for (offset, name) in [
        (4_usize, "project name"),
        (8, "config name"),
        (40, "internal name"),
        (100, "display name"),
    ] {
        let left_pointer = u32::from_le_bytes(left[offset..offset + 4].try_into().unwrap());
        let right_pointer = u32::from_le_bytes(right[offset..offset + 4].try_into().unwrap());
        let left_string = expected.string(left_pointer, &format!("GEN8 {name}"))?;
        let right_string = actual.string(right_pointer, &format!("GEN8 {name}"))?;
        if left_string != right_string {
            differences.push(format!(
                "GEN8 {name} differs: expected {left_string:?}, actual {right_string:?}"
            ));
        }
        left[offset..offset + 4].fill(0);
        right[offset..offset + 4].fill(0);
    }
    // License CRC/MD5 and the build timestamp describe compiler provenance,
    // not game behaviour. Official builds intentionally vary these bytes.
    left[72..100].fill(0);
    right[72..100].fill(0);
    if left != right {
        differences.push(payload_difference(GEN8, &left, &right));
    }
    Ok(())
}

fn compare_textures(
    expected: &ParsedWad,
    expected_chunk: &Chunk,
    actual: &ParsedWad,
    actual_chunk: &Chunk,
    differences: &mut Vec<String>,
) -> Result<(), SemanticDiffError> {
    let left = parse_textures(expected, expected_chunk)?;
    let right = parse_textures(actual, actual_chunk)?;
    if left.len() != right.len() {
        differences.push(format!(
            "TXTR count differs: expected {}, actual {}",
            left.len(),
            right.len()
        ));
        return Ok(());
    }
    for (index, (left, right)) in left.iter().zip(&right).enumerate() {
        if left.scaled != right.scaled {
            differences.push(format!(
                "TXTR[{index}].scaled differs: expected {}, actual {}",
                left.scaled, right.scaled
            ));
        }
        if left.width != right.width || left.height != right.height {
            differences.push(format!(
                "TXTR[{index}] dimensions differ: expected {}x{}, actual {}x{}",
                left.width, left.height, right.width, right.height
            ));
        } else if left.rgba != right.rgba {
            let pixels = left
                .rgba
                .chunks_exact(4)
                .zip(right.rgba.chunks_exact(4))
                .filter(|(left, right)| left != right)
                .count();
            differences.push(format!("TXTR[{index}] differs in {pixels} RGBA pixels"));
        }
    }
    Ok(())
}

struct TextureImage {
    scaled: i32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn parse_textures(file: &ParsedWad, chunk: &Chunk) -> Result<Vec<TextureImage>, SemanticDiffError> {
    let count = usize::try_from(file.read_u32(chunk.data_offset, "TXTR count")?)
        .map_err(|_| file.malformed("TXTR count does not fit usize"))?;
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let record = u64::from(file.read_u32(
            chunk.data_offset + 4 + index as u64 * 4,
            "TXTR record pointer",
        )?);
        let scaled = file.read_i32(record, "TXTR scaled")?;
        let data = u64::from(file.read_u32(record + 4, "TXTR PNG pointer")?);
        if data < chunk.data_offset || data >= chunk.end_offset() {
            return Err(file.malformed(format!(
                "TXTR[{index}] PNG pointer {data:#x} is outside TXTR"
            )));
        }
        let end = png_end(file, data, chunk.end_offset(), index)?;
        let image = image::load_from_memory_with_format(
            file.range(data, end - data, "TXTR PNG")?,
            image::ImageFormat::Png,
        )
        .map_err(|source| SemanticDiffError::Image {
            label: file.label,
            index,
            source,
        })?
        .into_rgba8();
        result.push(TextureImage {
            scaled,
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        });
    }
    Ok(result)
}

fn png_end(
    file: &ParsedWad,
    start: u64,
    limit: u64,
    index: usize,
) -> Result<u64, SemanticDiffError> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if file.range(start, 8, "PNG signature")? != SIGNATURE {
        return Err(file.malformed(format!("TXTR[{index}] is not a PNG image")));
    }
    let mut offset = start + 8;
    loop {
        if offset + 12 > limit {
            return Err(file.malformed(format!("TXTR[{index}] PNG is truncated")));
        }
        let header = file.range(offset, 8, "PNG chunk header")?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as u64;
        let kind = &header[4..8];
        offset = offset
            .checked_add(12 + length)
            .filter(|offset| *offset <= limit)
            .ok_or_else(|| file.malformed(format!("TXTR[{index}] PNG chunk is truncated")))?;
        if kind == b"IEND" {
            return Ok(offset);
        }
    }
}

fn compare_audio(
    expected: &ParsedWad,
    expected_chunk: &Chunk,
    actual: &ParsedWad,
    actual_chunk: &Chunk,
    differences: &mut Vec<String>,
) -> Result<(), SemanticDiffError> {
    let left = parse_audio(expected, expected_chunk)?;
    let right = parse_audio(actual, actual_chunk)?;
    if left.len() != right.len() {
        differences.push(format!(
            "AUDO count differs: expected {}, actual {}",
            left.len(),
            right.len()
        ));
        return Ok(());
    }
    for (index, (left, right)) in left.iter().zip(&right).enumerate() {
        if left == right {
            continue;
        }
        let equivalent = if left.starts_with(b"OggS") && right.starts_with(b"OggS") {
            normalize_ogg(expected, left, index)? == normalize_ogg(actual, right, index)?
        } else {
            false
        };
        if !equivalent {
            differences.push(format!(
                "AUDO[{index}] differs: expected {} bytes, actual {} bytes",
                left.len(),
                right.len()
            ));
        }
    }
    Ok(())
}

fn parse_audio<'a>(file: &'a ParsedWad, chunk: &Chunk) -> Result<Vec<&'a [u8]>, SemanticDiffError> {
    let count = usize::try_from(file.read_u32(chunk.data_offset, "AUDO count")?)
        .map_err(|_| file.malformed("AUDO count does not fit usize"))?;
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let record = u64::from(file.read_u32(
            chunk.data_offset + 4 + index as u64 * 4,
            "AUDO record pointer",
        )?);
        let size = u64::from(file.read_u32(record, "AUDO blob size")?);
        let data = record + 4;
        if data < chunk.data_offset || data + size > chunk.end_offset() {
            return Err(file.malformed(format!(
                "AUDO[{index}] range {data:#x}..{:#x} is outside AUDO",
                data + size
            )));
        }
        result.push(file.range(data, size, "AUDO blob")?);
    }
    Ok(result)
}

fn normalize_ogg(
    file: &ParsedWad,
    input: &[u8],
    index: usize,
) -> Result<Vec<u8>, SemanticDiffError> {
    let mut output = input.to_vec();
    let mut offset = 0_usize;
    while offset < output.len() {
        if output.len() - offset < 27 || &output[offset..offset + 4] != b"OggS" {
            return Err(file.malformed(format!("AUDO[{index}] contains a malformed Ogg page")));
        }
        let segments = output[offset + 26] as usize;
        let header_end = offset + 27 + segments;
        if header_end > output.len() {
            return Err(file.malformed(format!("AUDO[{index}] has a truncated Ogg header")));
        }
        let body = output[offset + 27..header_end]
            .iter()
            .map(|value| usize::from(*value))
            .sum::<usize>();
        let page_end = header_end
            .checked_add(body)
            .filter(|end| *end <= output.len())
            .ok_or_else(|| file.malformed(format!("AUDO[{index}] has a truncated Ogg body")))?;
        output[offset + 14..offset + 18].fill(0);
        output[offset + 22..offset + 26].fill(0);
        offset = page_end;
    }
    Ok(output)
}

#[derive(Debug, PartialEq, Eq)]
struct VmSemantic {
    strings: Vec<String>,
    codes: Vec<CodeSemantic>,
    variables: Vec<VariableSemantic>,
    functions: Vec<FunctionSemantic>,
    locals: Vec<LocalTableSemantic>,
    variable_header: [i32; 3],
}

#[derive(Debug, PartialEq, Eq)]
struct CodeSemantic {
    name: String,
    locals: u16,
    flags: u16,
    trailing: i32,
    bytecode: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
struct VariableSemantic {
    name: String,
    instance: i32,
    index: i32,
    references: u32,
    first: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
struct FunctionSemantic {
    name: String,
    references: u32,
    first: Option<u64>,
}

type ReferenceTerminals = HashMap<u64, u32>;
type ParsedVariables = ([i32; 3], Vec<VariableSemantic>, ReferenceTerminals);
type ParsedFunctions = (
    Vec<FunctionSemantic>,
    Vec<LocalTableSemantic>,
    ReferenceTerminals,
);

#[derive(Debug, PartialEq, Eq)]
struct LocalTableSemantic {
    code: String,
    locals: Vec<(u32, String)>,
}

fn compare_vm(
    expected: &ParsedWad,
    actual: &ParsedWad,
    differences: &mut Vec<String>,
) -> Result<(), SemanticDiffError> {
    let left = parse_vm(expected)?;
    let right = parse_vm(actual)?;
    if left.strings != right.strings {
        differences.push(first_vector_difference(
            "STRG",
            &left.strings,
            &right.strings,
        ));
    }
    if left.variable_header != right.variable_header {
        differences.push(format!(
            "VARI header differs: expected {:?}, actual {:?}",
            left.variable_header, right.variable_header
        ));
    }
    if left.variables != right.variables {
        differences.push(first_vector_difference(
            "VARI records",
            &left.variables,
            &right.variables,
        ));
    }
    if left.functions != right.functions {
        differences.push(first_vector_difference(
            "FUNC records",
            &left.functions,
            &right.functions,
        ));
    }
    if left.locals != right.locals {
        differences.push(first_vector_difference(
            "FUNC local tables",
            &left.locals,
            &right.locals,
        ));
    }
    if left.codes.len() != right.codes.len() {
        differences.push(format!(
            "CODE count differs: expected {}, actual {}",
            left.codes.len(),
            right.codes.len()
        ));
    } else {
        for (index, (left, right)) in left.codes.iter().zip(&right.codes).enumerate() {
            if left == right {
                continue;
            }
            if left.name != right.name {
                differences.push(format!(
                    "CODE[{index}] name differs: expected {:?}, actual {:?}",
                    left.name, right.name
                ));
            } else if left.locals != right.locals
                || left.flags != right.flags
                || left.trailing != right.trailing
            {
                differences.push(format!("CODE[{index}] {:?} metadata differs", left.name));
            } else {
                let detail = byte_difference(&left.bytecode, &right.bytecode);
                differences.push(format!("CODE[{index}] {:?} bytecode {detail}", left.name));
            }
            break;
        }
    }
    Ok(())
}

fn parse_vm(file: &ParsedWad) -> Result<VmSemantic, SemanticDiffError> {
    let code = file.chunk(CODE).unwrap();
    let vari = file.chunk(VARI).unwrap();
    let func = file.chunk(FUNC).unwrap();
    let strg = file.chunk(STRG).unwrap();
    let strings = parse_strings(file, strg)?;
    let (variable_header, variables, mut terminals) = parse_variables(file, vari, code)?;
    let (functions, locals, function_terminals) = parse_functions(file, func, code)?;
    terminals.extend(function_terminals);
    let codes = parse_codes(file, code, &terminals)?;
    Ok(VmSemantic {
        strings,
        codes,
        variables,
        functions,
        locals,
        variable_header,
    })
}

fn parse_strings(file: &ParsedWad, chunk: &Chunk) -> Result<Vec<String>, SemanticDiffError> {
    let count = usize::try_from(file.read_u32(chunk.data_offset, "STRG count")?)
        .map_err(|_| file.malformed("STRG count does not fit usize"))?;
    (0..count)
        .map(|index| {
            let record =
                u64::from(file.read_u32(chunk.data_offset + 4 + index as u64 * 4, "STRG pointer")?);
            let length = u64::from(file.read_u32(record, "STRG length")?);
            let bytes = file.range(record + 4, length, "STRG bytes")?;
            String::from_utf8(bytes.to_vec())
                .map_err(|source| file.malformed(format!("STRG[{index}] is not UTF-8: {source}")))
        })
        .collect()
}

fn parse_variables(
    file: &ParsedWad,
    chunk: &Chunk,
    code: &Chunk,
) -> Result<ParsedVariables, SemanticDiffError> {
    if chunk.size < 12 {
        return Err(file.malformed("VARI is shorter than its 12-byte header"));
    }
    let header = [
        file.read_i32(chunk.data_offset, "VARI instance count")?,
        file.read_i32(chunk.data_offset + 4, "VARI global count")?,
        file.read_i32(chunk.data_offset + 8, "VARI max locals")?,
    ];
    let records_size = chunk.size - 12;
    let count = (records_size / 20) as usize;
    let trailing = records_size % 20;
    let padding = file.range(
        chunk.data_offset + 12 + count as u64 * 20,
        u64::from(trailing),
        "VARI padding",
    )?;
    if trailing >= 16 || padding.iter().any(|byte| *byte != 0) {
        return Err(file.malformed(format!("VARI has {trailing} non-padding trailing bytes")));
    }
    let mut records = Vec::with_capacity(count);
    let mut terminals = HashMap::new();
    for index in 0..count {
        let offset = chunk.data_offset + 12 + index as u64 * 20;
        let name = file.string(file.read_u32(offset, "VARI name")?, "VARI name")?;
        let instance = file.read_i32(offset + 4, "VARI instance")?;
        let variable_index = file.read_i32(offset + 8, "VARI index")?;
        let references = file.read_u32(offset + 12, "VARI reference count")?;
        let first_absolute = file.read_u32(offset + 16, "VARI first reference")?;
        let first = (references != 0).then(|| {
            u64::from(first_absolute)
                .checked_sub(code.data_offset)
                .ok_or_else(|| file.malformed(format!("VARI[{index}] reference precedes CODE")))
        });
        let first = match first {
            Some(value) => Some(value?),
            None => None,
        };
        collect_terminal(
            file,
            first_absolute,
            references,
            true,
            index,
            &mut terminals,
        )?;
        records.push(VariableSemantic {
            name,
            instance,
            index: variable_index,
            references,
            first,
        });
    }
    Ok((header, records, terminals))
}

fn parse_functions(
    file: &ParsedWad,
    chunk: &Chunk,
    code: &Chunk,
) -> Result<ParsedFunctions, SemanticDiffError> {
    let count = usize::try_from(file.read_u32(chunk.data_offset, "FUNC count")?)
        .map_err(|_| file.malformed("FUNC count does not fit usize"))?;
    let records_end = chunk.data_offset + 4 + count as u64 * 12;
    if records_end > chunk.end_offset() {
        return Err(file.malformed("FUNC records exceed the chunk"));
    }
    let mut records = Vec::with_capacity(count);
    let mut terminals = HashMap::new();
    for index in 0..count {
        let offset = chunk.data_offset + 4 + index as u64 * 12;
        let name = file.string(file.read_u32(offset, "FUNC name")?, "FUNC name")?;
        let references = file.read_u32(offset + 4, "FUNC reference count")?;
        let first_absolute = file.read_u32(offset + 8, "FUNC first reference")?;
        let first = if references == 0 {
            None
        } else {
            Some(
                u64::from(first_absolute)
                    .checked_sub(code.data_offset)
                    .ok_or_else(|| file.malformed(format!("FUNC[{index}] precedes CODE")))?,
            )
        };
        collect_terminal(
            file,
            first_absolute,
            references,
            false,
            index,
            &mut terminals,
        )?;
        records.push(FunctionSemantic {
            name,
            references,
            first,
        });
    }

    let table_count = usize::try_from(file.read_u32(records_end, "FUNC local table count")?)
        .map_err(|_| file.malformed("FUNC local table count does not fit usize"))?;
    let mut offset = records_end + 4;
    let mut locals = Vec::with_capacity(table_count);
    for table in 0..table_count {
        let count = usize::try_from(file.read_u32(offset, "FUNC local count")?)
            .map_err(|_| file.malformed("FUNC local count does not fit usize"))?;
        let code_name = file.string(file.read_u32(offset + 4, "FUNC code name")?, "FUNC code")?;
        offset += 8;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let slot = file.read_u32(offset, "FUNC local slot")?;
            let name = file.string(file.read_u32(offset + 4, "FUNC local name")?, "FUNC local")?;
            entries.push((slot, name));
            offset += 8;
        }
        if offset > chunk.end_offset() {
            return Err(file.malformed(format!("FUNC local table {table} exceeds the chunk")));
        }
        locals.push(LocalTableSemantic {
            code: code_name,
            locals: entries,
        });
    }
    if offset != chunk.end_offset() {
        let trailing = chunk.end_offset() - offset;
        let padding = file.range(offset, trailing, "FUNC padding")?;
        if trailing >= 16 || padding.iter().any(|byte| *byte != 0) {
            return Err(file.malformed(format!("FUNC has {trailing} non-padding trailing bytes")));
        }
    }
    Ok((records, locals, terminals))
}

fn collect_terminal(
    file: &ParsedWad,
    first: u32,
    count: u32,
    variable: bool,
    index: usize,
    terminals: &mut HashMap<u64, u32>,
) -> Result<(), SemanticDiffError> {
    if count == 0 {
        return Ok(());
    }
    let mut instruction = u64::from(first);
    for reference in 0..count {
        let payload = instruction + 4;
        let word = file.read_u32(payload, "VM reference link")?;
        if reference + 1 == count {
            terminals.insert(payload, if variable { 0xf000_0000 } else { 0 });
            break;
        }
        let delta = if variable { word & 0x0fff_ffff } else { word };
        if delta == 0 {
            return Err(file.malformed(format!(
                "{}[{index}] reference chain ends before its declared count",
                if variable { "VARI" } else { "FUNC" }
            )));
        }
        instruction = instruction
            .checked_add(u64::from(delta))
            .ok_or_else(|| file.malformed("VM reference chain overflows"))?;
    }
    Ok(())
}

fn parse_codes(
    file: &ParsedWad,
    chunk: &Chunk,
    terminals: &HashMap<u64, u32>,
) -> Result<Vec<CodeSemantic>, SemanticDiffError> {
    let count = usize::try_from(file.read_u32(chunk.data_offset, "CODE count")?)
        .map_err(|_| file.malformed("CODE count does not fit usize"))?;
    let table_end = chunk.data_offset + 4 + count as u64 * 4;
    if table_end > chunk.end_offset() {
        return Err(file.malformed("CODE offset table exceeds the chunk"));
    }
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let record = u64::from(file.read_u32(
            chunk.data_offset + 4 + index as u64 * 4,
            "CODE record pointer",
        )?);
        if record < table_end || record + 20 > chunk.end_offset() {
            return Err(file.malformed(format!("CODE[{index}] record is outside CODE")));
        }
        let name = file.string(file.read_u32(record, "CODE name")?, "CODE name")?;
        let length = u64::from(file.read_u32(record + 4, "CODE bytecode length")?);
        let locals = file.read_u16(record + 8, "CODE local count")?;
        let flags = file.read_u16(record + 10, "CODE flags")?;
        let relative = i64::from(file.read_i32(record + 12, "CODE bytecode offset")?);
        let base = (record + 12)
            .checked_add_signed(relative)
            .ok_or_else(|| file.malformed(format!("CODE[{index}] bytecode offset overflows")))?;
        if base < table_end || base + length > chunk.end_offset() {
            return Err(file.malformed(format!("CODE[{index}] bytecode is outside CODE")));
        }
        let trailing = file.read_i32(record + 16, "CODE trailing field")?;
        let mut bytecode = file.range(base, length, "CODE bytecode")?.to_vec();
        for (&payload, &mask) in terminals {
            if payload < base || payload + 4 > base + length {
                continue;
            }
            let offset = (payload - base) as usize;
            let word = u32::from_le_bytes(bytecode[offset..offset + 4].try_into().unwrap()) & mask;
            bytecode[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        result.push(CodeSemantic {
            name,
            locals,
            flags,
            trailing,
            bytecode,
        });
    }
    Ok(result)
}

fn first_vector_difference<T: fmt::Debug + PartialEq>(
    name: &str,
    expected: &[T],
    actual: &[T],
) -> String {
    if expected.len() != actual.len() {
        return format!(
            "{name} count differs: expected {}, actual {}",
            expected.len(),
            actual.len()
        );
    }
    let index = expected
        .iter()
        .zip(actual)
        .position(|(left, right)| left != right)
        .unwrap();
    format!(
        "{name}[{index}] differs: expected {:?}, actual {:?}",
        expected[index], actual[index]
    )
}

fn payload_difference(name: FourCc, expected: &[u8], actual: &[u8]) -> String {
    format!("chunk {name} payload {}", byte_difference(expected, actual))
}

fn byte_difference(expected: &[u8], actual: &[u8]) -> String {
    let common = expected.len().min(actual.len());
    let first = (0..common)
        .find(|index| expected[*index] != actual[*index])
        .unwrap_or(common);
    let differing = expected[..common]
        .iter()
        .zip(&actual[..common])
        .filter(|(left, right)| left != right)
        .count()
        + expected.len().abs_diff(actual.len());
    format!(
        "differs at +{first:#x} ({differing} bytes; expected {}, actual {})",
        expected.len(),
        actual.len()
    )
}

fn four_cc_list(chunks: &[FourCc]) -> String {
    chunks
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug)]
pub enum SemanticDiffError {
    Io(io::Error),
    InvalidWad {
        label: &'static str,
        source: ReadError,
    },
    Malformed {
        label: &'static str,
        message: String,
    },
    Image {
        label: &'static str,
        index: usize,
        source: ImageError,
    },
}

impl fmt::Display for SemanticDiffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => source.fmt(formatter),
            Self::InvalidWad { label, source } => {
                write!(formatter, "invalid {label} WAD: {source}")
            }
            Self::Malformed { label, message } => {
                write!(formatter, "malformed {label} WAD: {message}")
            }
            Self::Image {
                label,
                index,
                source,
            } => write!(formatter, "cannot decode {label} TXTR[{index}]: {source}"),
        }
    }
}

impl Error for SemanticDiffError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::InvalidWad { source, .. } => Some(source),
            Self::Image { source, .. } => Some(source),
            Self::Malformed { .. } => None,
        }
    }
}

impl From<io::Error> for SemanticDiffError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wad::WadBuilder;

    #[test]
    fn compares_identical_and_changed_regular_chunks() {
        let mut expected = simple_wad(7);
        let mut same = simple_wad(7);
        let mut changed = simple_wad(8);

        assert!(
            diff_wads_semantic(&mut expected, &mut same)
                .unwrap()
                .is_equivalent()
        );
        assert!(
            !diff_wads_semantic(&mut expected, &mut changed)
                .unwrap()
                .is_equivalent()
        );
    }

    #[test]
    fn ogg_normalization_ignores_only_serials_and_checksums() {
        let file = ParsedWad {
            label: "test",
            bytes: Vec::new(),
            wad: WadFile {
                form_size: 0,
                file_size: 0,
                chunks: Vec::new(),
            },
        };
        let left = ogg_page(1, 2, b"abc");
        let right = ogg_page(3, 4, b"abc");
        let changed = ogg_page(3, 4, b"abd");

        assert_eq!(
            normalize_ogg(&file, &left, 0).unwrap(),
            normalize_ogg(&file, &right, 0).unwrap()
        );
        assert_ne!(
            normalize_ogg(&file, &left, 0).unwrap(),
            normalize_ogg(&file, &changed, 0).unwrap()
        );
    }

    fn simple_wad(value: u32) -> Cursor<Vec<u8>> {
        let mut builder = WadBuilder::new();
        builder
            .add_chunk(FourCc::new(*b"TEST"), move |writer| writer.write_u32(value))
            .unwrap();
        let mut output = Cursor::new(Vec::new());
        builder.write_to(&mut output).unwrap();
        output.set_position(0);
        output
    }

    fn ogg_page(serial: u32, checksum: u32, body: &[u8]) -> Vec<u8> {
        let mut page = vec![0_u8; 28];
        page[..4].copy_from_slice(b"OggS");
        page[14..18].copy_from_slice(&serial.to_le_bytes());
        page[22..26].copy_from_slice(&checksum.to_le_bytes());
        page[26] = 1;
        page[27] = u8::try_from(body.len()).unwrap();
        page.extend_from_slice(body);
        page
    }
}
