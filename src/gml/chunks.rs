use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use crate::wad::{ChunkWriter, FourCc, StringTable, WadBuilder, WriteError};

use super::{CompiledProject, VariableKind, builtins};

const CODE: FourCc = FourCc::new(*b"CODE");
const VARI: FourCc = FourCc::new(*b"VARI");
const FUNC: FourCc = FourCc::new(*b"FUNC");

#[derive(Debug, Default)]
struct VmLayout {
    code_bases: Option<Vec<u32>>,
}

/// Adds the three GMS 1.4 VM chunks in official order. CODE records the
/// absolute bytecode bases; VARI and FUNC subsequently use those bases to
/// patch their linked reference lists in-place.
pub fn add_vm_chunks<'a>(
    builder: &mut WadBuilder<'a>,
    strings: &'a StringTable,
    project: &'a CompiledProject,
) -> Result<(), WriteError> {
    intern_vm_strings(strings, project)?;
    let layout = Rc::new(RefCell::new(VmLayout::default()));
    let code_layout = Rc::clone(&layout);
    builder.add_chunk(CODE, move |writer| {
        write_code(writer, strings, project, &mut code_layout.borrow_mut())
    })?;
    let variable_layout = Rc::clone(&layout);
    builder.add_chunk(VARI, move |writer| {
        write_variables(writer, strings, project, &variable_layout.borrow())
    })?;
    builder.add_chunk(FUNC, move |writer| {
        write_functions(writer, strings, project, &layout.borrow())
    })?;
    Ok(())
}

fn intern_vm_strings(strings: &StringTable, project: &CompiledProject) -> Result<(), WriteError> {
    // Every official GML2VM instance registers these names before compiling.
    // The shared string list de-duplicates them, so one initial registration
    // produces the same order for the whole project.
    strings.intern("prototype")?;
    strings.intern("@@array@@")?;
    strings.intern("arguments")?;

    for code in &project.codes {
        let mut variable = 0;
        let mut function = 0;
        let mut string = 0;
        loop {
            let variable_offset = code
                .bytecode
                .variable_references
                .get(variable)
                .map(|reference| reference.offset);
            let function_offset = code
                .bytecode
                .function_references
                .get(function)
                .map(|reference| reference.offset);
            let string_offset = code
                .bytecode
                .string_references
                .get(string)
                .map(|reference| reference.offset);
            let next = [variable_offset, function_offset, string_offset]
                .into_iter()
                .flatten()
                .min();
            let Some(next) = next else {
                break;
            };
            if variable_offset == Some(next) {
                strings.intern(&code.bytecode.variable_references[variable].name)?;
                variable += 1;
            } else if function_offset == Some(next) {
                strings.intern(&code.bytecode.function_references[function].name)?;
                function += 1;
            } else {
                strings.intern(&code.bytecode.string_references[string].value)?;
                string += 1;
            }
        }
    }
    Ok(())
}

fn write_code(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    project: &CompiledProject,
    layout: &mut VmLayout,
) -> Result<(), WriteError> {
    let count = as_u32(project.codes.len(), "CODE record count")?;
    writer.write_u32(count)?;
    let mut records = Vec::with_capacity(project.codes.len());
    for _ in &project.codes {
        records.push(writer.reserve_u32()?);
    }

    let mut bases = Vec::with_capacity(project.codes.len());
    for (code_index, code) in project.codes.iter().enumerate() {
        if code.bytecode.bytes.len() & 3 != 0 {
            return Err(invalid_vm(format!(
                "CODE entry {code_index} ({}) has a byte length that is not word-aligned",
                code.vm_name
            )));
        }
        let base = writer.position_u32()?;
        bases.push(base);
        writer.write_all(&code.bytecode.bytes)?;
        for reference in &code.bytecode.string_references {
            validate_reference(
                code_index,
                code.bytecode.bytes.len(),
                reference.offset,
                "string",
            )?;
            let index = strings.intern(&reference.value)?;
            writer.patch_u32_at(
                u64::from(base) + u64::from(reference.offset),
                as_u32(index, "VM string index")?,
            )?;
        }
    }
    writer.align(4)?;

    for (((record, code), base), code_index) in records
        .into_iter()
        .zip(&project.codes)
        .zip(&bases)
        .zip(0_usize..)
    {
        writer.patch_position(record)?;
        strings.write_reference(writer, &code.vm_name)?;
        writer.write_u32(as_u32(code.bytecode.bytes.len(), "CODE bytecode length")?)?;
        let local_count = u16::try_from(code.local_count).map_err(|_| {
            invalid_vm(format!(
                "CODE entry {code_index} ({}) has more than 65535 locals",
                code.vm_name
            ))
        })?;
        writer.write_u16(local_count)?;
        let flags = if code.local_count == 0 { 4_u16 } else { 0 };
        writer.write_u16(flags << 13)?;
        let relative_field = writer.position()?;
        let relative = i64::from(*base) - relative_field as i64;
        writer.write_i32(i32::try_from(relative).map_err(|_| {
            invalid_vm(format!(
                "CODE entry {code_index} ({}) relative offset does not fit i32",
                code.vm_name
            ))
        })?)?;
        writer.write_i32(0)?;
    }
    layout.code_bases = Some(bases);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct VariableKey {
    name: String,
    kind: VariableKind,
    code_index: Option<usize>,
}

#[derive(Debug)]
struct VariableEntry {
    key: VariableKey,
    instance: i32,
    index: i32,
    references: Vec<ReferenceLocation>,
}

#[derive(Debug, Clone, Copy)]
struct ReferenceLocation {
    instruction: u32,
    payload: u32,
    flags: u32,
}

struct VariableTable {
    entries: Vec<VariableEntry>,
    indices: HashMap<VariableKey, usize>,
    next_instance_index: i32,
}

impl VariableTable {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            indices: HashMap::new(),
            next_instance_index: 0,
        }
    }

    fn ensure(&mut self, key: VariableKey, local_slot: Option<u32>) -> Result<usize, WriteError> {
        if let Some(index) = self.indices.get(&key) {
            return Ok(*index);
        }
        let (instance, index) = match key.kind {
            VariableKind::Global => {
                let index = self.take_instance_index()?;
                (-5, index)
            }
            VariableKind::Instance => {
                if builtins::is_variable(&key.name) {
                    (-1, -6)
                } else {
                    let index = self.take_instance_index()?;
                    (-1, index)
                }
            }
            VariableKind::Builtin => (-1, -6),
            VariableKind::Local => {
                let slot = local_slot.ok_or_else(|| {
                    invalid_vm(format!("local variable {} has no slot", key.name))
                })?;
                let slot = i32::try_from(slot)
                    .map_err(|_| invalid_vm(format!("local slot {slot} does not fit i32")))?;
                (-7, slot)
            }
            VariableKind::Unknown => (-6, 0),
        };
        let entry_index = self.entries.len();
        self.entries.push(VariableEntry {
            key: key.clone(),
            instance,
            index,
            references: Vec::new(),
        });
        self.indices.insert(key, entry_index);
        Ok(entry_index)
    }

    fn take_instance_index(&mut self) -> Result<i32, WriteError> {
        let value = self.next_instance_index;
        self.next_instance_index = self
            .next_instance_index
            .checked_add(1)
            .ok_or_else(|| invalid_vm("more than i32::MAX instance/global variables"))?;
        Ok(value)
    }
}

fn write_variables(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    project: &CompiledProject,
    layout: &VmLayout,
) -> Result<(), WriteError> {
    let bases = code_bases(layout, project.codes.len())?;
    let mut table = VariableTable::new();
    table.ensure(
        VariableKey {
            name: "prototype".to_owned(),
            kind: VariableKind::Instance,
            code_index: None,
        },
        None,
    )?;
    table.ensure(
        VariableKey {
            name: "@@array@@".to_owned(),
            kind: VariableKind::Instance,
            code_index: None,
        },
        None,
    )?;

    for (code_index, (code, base)) in project.codes.iter().zip(bases).enumerate() {
        if let Some(arguments) = code.locals.first() {
            table.ensure(
                VariableKey {
                    name: arguments.name.clone(),
                    kind: VariableKind::Local,
                    code_index: Some(code_index),
                },
                Some(arguments.slot),
            )?;
        }
        let mut groups = Vec::<Vec<usize>>::new();
        let mut group_indices = HashMap::<&str, usize>::new();
        for (reference_index, reference) in code.bytecode.variable_references.iter().enumerate() {
            let group = if let Some(group) = group_indices.get(reference.name.as_str()) {
                *group
            } else {
                let group = groups.len();
                groups.push(Vec::new());
                group_indices.insert(reference.name.as_str(), group);
                group
            };
            groups[group].push(reference_index);
        }
        for group in groups {
            for reference_index in group {
                let reference = &code.bytecode.variable_references[reference_index];
                let kind = if reference.kind == VariableKind::Builtin {
                    VariableKind::Instance
                } else {
                    reference.kind
                };
                let key = VariableKey {
                    name: reference.name.clone(),
                    kind,
                    code_index: (kind == VariableKind::Local).then_some(code_index),
                };
                let entry = table.ensure(key, reference.local_slot)?;
                table.entries[entry].references.push(reference_location(
                    code_index,
                    code.bytecode.bytes.len(),
                    *base,
                    reference.offset,
                    reference.flags,
                    "variable",
                )?);
            }
        }
    }

    writer.write_i32(table.next_instance_index)?;
    writer.write_i32(table.next_instance_index)?;
    let max_locals = project
        .codes
        .iter()
        .map(|code| code.locals.len())
        .max()
        .unwrap_or(0);
    writer.write_i32(
        i32::try_from(max_locals).map_err(|_| {
            invalid_vm(format!("maximum local count {max_locals} does not fit i32"))
        })?,
    )?;

    for entry in &table.entries {
        strings.write_reference(writer, &entry.key.name)?;
        writer.write_i32(entry.instance)?;
        writer.write_i32(entry.index)?;
        writer.write_u32(as_u32(entry.references.len(), "VARI reference count")?)?;
        if let Some(first) = entry.references.first() {
            writer.write_u32(first.instruction)?;
        } else {
            writer.write_i32(-1)?;
        }
        patch_variable_links(writer, &entry.references)?;
    }
    Ok(())
}

fn patch_variable_links(
    writer: &mut ChunkWriter<'_>,
    references: &[ReferenceLocation],
) -> Result<(), WriteError> {
    for pair in references.windows(2) {
        let current = pair[0];
        let next = pair[1];
        let delta = next
            .instruction
            .checked_sub(current.instruction)
            .ok_or_else(|| invalid_vm("VARI references are not in ascending order"))?;
        if delta > 0x0fff_ffff {
            return Err(invalid_vm(format!(
                "VARI reference link from {:#x} to {:#x} exceeds 28 bits",
                current.instruction, next.instruction
            )));
        }
        writer.patch_u32_at(
            u64::from(current.payload),
            (current.flags & 0xf000_0000) | delta,
        )?;
    }
    Ok(())
}

#[derive(Debug)]
struct FunctionEntry {
    name: String,
    references: Vec<ReferenceLocation>,
}

fn write_functions(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    project: &CompiledProject,
    layout: &VmLayout,
) -> Result<(), WriteError> {
    let bases = code_bases(layout, project.codes.len())?;
    let mut entries = Vec::<FunctionEntry>::new();
    let mut indices = HashMap::<String, usize>::new();
    for (code_index, (code, base)) in project.codes.iter().zip(bases).enumerate() {
        for reference in &code.bytecode.function_references {
            let entry = if let Some(index) = indices.get(&reference.name) {
                *index
            } else {
                let index = entries.len();
                entries.push(FunctionEntry {
                    name: reference.name.clone(),
                    references: Vec::new(),
                });
                indices.insert(reference.name.clone(), index);
                index
            };
            entries[entry].references.push(reference_location(
                code_index,
                code.bytecode.bytes.len(),
                *base,
                reference.offset,
                0,
                "function",
            )?);
        }
    }

    writer.write_u32(as_u32(entries.len(), "FUNC record count")?)?;
    for entry in &entries {
        strings.write_reference(writer, &entry.name)?;
        writer.write_u32(as_u32(entry.references.len(), "FUNC reference count")?)?;
        writer.write_u32(
            entry
                .references
                .first()
                .expect("function entries always have a reference")
                .instruction,
        )?;
        patch_function_links(writer, &entry.references)?;
    }

    writer.write_u32(as_u32(project.codes.len(), "FUNC local table count")?)?;
    for code in &project.codes {
        writer.write_u32(as_u32(code.locals.len(), "FUNC local count")?)?;
        strings.write_reference(writer, &code.vm_name)?;
        for local in &code.locals {
            writer.write_u32(local.slot)?;
            strings.write_reference(writer, &local.name)?;
        }
    }
    Ok(())
}

fn patch_function_links(
    writer: &mut ChunkWriter<'_>,
    references: &[ReferenceLocation],
) -> Result<(), WriteError> {
    for pair in references.windows(2) {
        let current = pair[0];
        let next = pair[1];
        let delta = next
            .instruction
            .checked_sub(current.instruction)
            .ok_or_else(|| invalid_vm("FUNC references are not in ascending order"))?;
        writer.patch_u32_at(u64::from(current.payload), delta)?;
    }
    Ok(())
}

fn reference_location(
    code_index: usize,
    code_length: usize,
    base: u32,
    offset: u32,
    flags: u32,
    kind: &'static str,
) -> Result<ReferenceLocation, WriteError> {
    validate_reference(code_index, code_length, offset, kind)?;
    let payload = base
        .checked_add(offset)
        .ok_or_else(|| invalid_vm(format!("{kind} reference address overflows u32")))?;
    Ok(ReferenceLocation {
        instruction: payload - 4,
        payload,
        flags,
    })
}

fn validate_reference(
    code_index: usize,
    code_length: usize,
    offset: u32,
    kind: &'static str,
) -> Result<(), WriteError> {
    let end = usize::try_from(offset)
        .ok()
        .and_then(|offset| offset.checked_add(4));
    if offset < 4 || end.is_none_or(|end| end > code_length) {
        return Err(invalid_vm(format!(
            "{kind} reference at byte {offset} is outside CODE entry {code_index} ({code_length} bytes)"
        )));
    }
    Ok(())
}

fn code_bases(layout: &VmLayout, count: usize) -> Result<&[u32], WriteError> {
    let bases = layout
        .code_bases
        .as_deref()
        .ok_or_else(|| invalid_vm("CODE must be serialized before VARI and FUNC"))?;
    if bases.len() != count {
        return Err(invalid_vm("CODE base count does not match compiled code"));
    }
    Ok(bases)
}

fn as_u32(value: usize, field: &'static str) -> Result<u32, WriteError> {
    u32::try_from(value).map_err(|_| WriteError::SizeOverflow {
        field,
        size: value as u64,
    })
}

fn invalid_vm(message: impl Into<String>) -> WriteError {
    WriteError::InvalidVmData {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::gml::{CodeKind, CompiledCode, LocalVariable, VmBuffer, VmSummary, VmType};
    use crate::wad::WadFile;

    #[test]
    fn writes_official_vm_records_and_reference_chains() {
        let mut vm = VmBuffer::new();
        for _ in 0..2 {
            vm.emit_variable(
                crate::gml::Opcode::Push,
                -1,
                None,
                None,
                "value",
                VariableKind::Instance,
                0xa000_0000,
                None,
            )
            .unwrap();
        }
        for _ in 0..2 {
            vm.emit_call("show_debug_message", 0).unwrap();
        }
        vm.emit_push_string("hello").unwrap();
        vm.emit(crate::gml::Opcode::Exit, VmType::Int);
        let bytecode = vm.finish().unwrap();
        let project = CompiledProject {
            function_classifications: 0,
            summary: VmSummary {
                code_units: 1,
                bytecode_bytes: bytecode.bytes.len(),
                variable_references: 2,
                function_references: 2,
                string_references: 1,
            },
            codes: vec![CompiledCode {
                kind: CodeKind::Script,
                name: "test".to_owned(),
                vm_name: "gml_Script_test".to_owned(),
                bytecode,
                local_count: 1,
                locals: vec![LocalVariable {
                    slot: 0,
                    name: "arguments".to_owned(),
                }],
            }],
        };

        let strings = StringTable::new();
        let mut builder = WadBuilder::new();
        add_vm_chunks(&mut builder, &strings, &project).unwrap();
        builder
            .add_chunk(FourCc::new(*b"STRG"), |writer| strings.write_strg(writer))
            .unwrap();
        let mut output = Cursor::new(Vec::new());
        let wad = builder.write_to(&mut output).unwrap();
        output.set_position(0);
        assert_eq!(WadFile::read(&mut output).unwrap(), wad);
        let bytes = output.into_inner();
        let code = wad.chunks.iter().find(|chunk| chunk.name == CODE).unwrap();
        let variables = wad.chunks.iter().find(|chunk| chunk.name == VARI).unwrap();
        let functions = wad.chunks.iter().find(|chunk| chunk.name == FUNC).unwrap();

        let code_data = code.data_offset as usize;
        assert_eq!(u32_at(&bytes, code_data), 1);
        let code_base = code_data + 8;
        let record = u32_at(&bytes, code_data + 4) as usize;
        assert_eq!(
            string_at(&bytes, u32_at(&bytes, record) as usize),
            "gml_Script_test"
        );
        assert_eq!(
            u32_at(&bytes, record + 4) as usize,
            project.codes[0].bytecode.bytes.len()
        );
        assert_eq!(u16_at(&bytes, record + 8), 1);
        assert_eq!(
            (record + 12) as i64 + i64::from(i32_at(&bytes, record + 12)),
            code_base as i64
        );

        let variable_data = variables.data_offset as usize;
        assert_eq!(i32_at(&bytes, variable_data), 3);
        assert_eq!(i32_at(&bytes, variable_data + 8), 1);
        let value_record = variable_data + 12 + 3 * 20;
        assert_eq!(
            string_at(&bytes, u32_at(&bytes, value_record) as usize),
            "value"
        );
        assert_eq!(i32_at(&bytes, value_record + 8), 2);
        assert_eq!(u32_at(&bytes, value_record + 12), 2);
        assert_eq!(u32_at(&bytes, value_record + 16) as usize, code_base);
        assert_eq!(u32_at(&bytes, code_base + 4), 0xa000_0008);

        let function_data = functions.data_offset as usize;
        assert_eq!(u32_at(&bytes, function_data), 1);
        assert_eq!(
            string_at(&bytes, u32_at(&bytes, function_data + 4) as usize),
            "show_debug_message"
        );
        assert_eq!(u32_at(&bytes, function_data + 8), 2);
        assert_eq!(u32_at(&bytes, function_data + 12) as usize, code_base + 16);
        assert_eq!(u32_at(&bytes, code_base + 20), 8);
        assert_eq!(u32_at(&bytes, code_base + 36), 5);
    }

    #[test]
    fn variable_records_group_types_by_first_name_occurrence() {
        let mut vm = VmBuffer::new();
        vm.emit_variable(
            crate::gml::Opcode::Push,
            -1,
            None,
            None,
            "shared",
            VariableKind::Instance,
            0xa000_0000,
            None,
        )
        .unwrap();
        vm.emit_variable(
            crate::gml::Opcode::Push,
            -1,
            None,
            None,
            "middle",
            VariableKind::Instance,
            0xa000_0000,
            None,
        )
        .unwrap();
        vm.emit_variable(
            crate::gml::Opcode::PushLocal,
            -7,
            None,
            None,
            "shared",
            VariableKind::Local,
            0xa000_0000,
            Some(1),
        )
        .unwrap();
        let bytecode = vm.finish().unwrap();
        let project = CompiledProject {
            function_classifications: 0,
            summary: VmSummary {
                code_units: 1,
                bytecode_bytes: bytecode.bytes.len(),
                variable_references: 3,
                function_references: 0,
                string_references: 0,
            },
            codes: vec![CompiledCode {
                kind: CodeKind::Script,
                name: "test".to_owned(),
                vm_name: "gml_Script_test".to_owned(),
                bytecode,
                local_count: 2,
                locals: vec![
                    LocalVariable {
                        slot: 0,
                        name: "arguments".to_owned(),
                    },
                    LocalVariable {
                        slot: 1,
                        name: "shared".to_owned(),
                    },
                ],
            }],
        };

        let strings = StringTable::new();
        let mut builder = WadBuilder::new();
        add_vm_chunks(&mut builder, &strings, &project).unwrap();
        builder
            .add_chunk(FourCc::new(*b"STRG"), |writer| strings.write_strg(writer))
            .unwrap();
        let mut output = Cursor::new(Vec::new());
        let wad = builder.write_to(&mut output).unwrap();
        let bytes = output.into_inner();
        let variables = wad.chunks.iter().find(|chunk| chunk.name == VARI).unwrap();
        let start = variables.data_offset as usize;
        assert_eq!(i32_at(&bytes, start), 4);
        let records = (0..6)
            .map(|index| {
                let record = start + 12 + index * 20;
                (
                    string_at(&bytes, u32_at(&bytes, record) as usize),
                    i32_at(&bytes, record + 4),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            records,
            [
                ("prototype", -1),
                ("@@array@@", -1),
                ("arguments", -7),
                ("shared", -1),
                ("shared", -7),
                ("middle", -1),
            ]
        );
    }

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn i32_at(bytes: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn string_at(bytes: &[u8], offset: usize) -> &str {
        let end = bytes[offset..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|length| offset + length)
            .unwrap();
        std::str::from_utf8(&bytes[offset..end]).unwrap()
    }
}
