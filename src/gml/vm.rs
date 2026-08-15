use std::fmt;

/// Value tags stored in the high argument byte of GMS 1.4 VM instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VmType {
    Double = 0,
    Float = 1,
    Int = 2,
    Long = 3,
    Bool = 4,
    Variable = 5,
    String = 6,
    Instance = 7,
    Delete = 8,
    Undefined = 9,
    UnsignedInt = 10,
    Error = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    PushV = 128,
    Push = 192,
    PushLocal = 193,
    PushGlobal = 194,
    PushBuiltin = 195,
    PushImmediate = 132,
    Pop = 69,
    PopV = 5,
    Dup = 134,
    Conv = 7,
    Mul = 8,
    Div = 9,
    Rem = 10,
    Mod = 11,
    Add = 12,
    Sub = 13,
    And = 14,
    Or = 15,
    Xor = 16,
    Neg = 17,
    Not = 18,
    Shl = 19,
    Shr = 20,
    Set = 21,
    Branch = 182,
    BranchTrue = 183,
    BranchFalse = 184,
    Call = 217,
    CallV = 153,
    PushEnv = 186,
    PopEnv = 187,
    Return = 156,
    Exit = 157,
    PopNull = 158,
    Break = 255,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Condition {
    None = 0,
    Less = 1,
    LessEqual = 2,
    Equal = 3,
    NotEqual = 4,
    GreaterEqual = 5,
    Greater = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VariableKind {
    Global,
    Instance,
    Local,
    Builtin,
    Unknown,
}

/// One word in the bytecode whose low 28 bits are linked by `VARI`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableReference {
    pub offset: u32,
    pub name: String,
    pub kind: VariableKind,
    pub flags: u32,
    pub local_slot: Option<u32>,
}

/// One word in the bytecode whose low 28 bits are linked by `FUNC`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionReference {
    pub offset: u32,
    pub name: String,
}

/// A string payload word patched to its zero-based entry index in `STRG`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringReference {
    pub offset: u32,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmBytecode {
    pub bytes: Vec<u8>,
    pub variable_references: Vec<VariableReference>,
    pub function_references: Vec<FunctionReference>,
    pub string_references: Vec<StringReference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Label(usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
    CodeTooLarge,
    LabelAlreadyMarked,
    UnmarkedLabel,
    BranchNotAligned { from: u32, to: u32 },
    BranchOutOfRange { from: u32, to: u32 },
}

impl fmt::Display for VmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CodeTooLarge => formatter.write_str("VM bytecode is larger than 4 GiB"),
            Self::LabelAlreadyMarked => formatter.write_str("VM label was marked more than once"),
            Self::UnmarkedLabel => formatter.write_str("VM bytecode contains an unmarked label"),
            Self::BranchNotAligned { from, to } => {
                write!(
                    formatter,
                    "VM branch from {from:#x} to {to:#x} is not 4-byte aligned"
                )
            }
            Self::BranchOutOfRange { from, to } => {
                write!(
                    formatter,
                    "VM branch from {from:#x} to {to:#x} exceeds 23 bits"
                )
            }
        }
    }
}

impl std::error::Error for VmError {}

#[derive(Debug, Clone, Copy)]
struct BranchPatch {
    offset: u32,
    opcode: Opcode,
    label: Label,
}

/// Little-endian instruction buffer using the exact 1.4 `VMBuffer` encoding.
#[derive(Debug, Default)]
pub struct VmBuffer {
    bytes: Vec<u8>,
    labels: Vec<Option<u32>>,
    branches: Vec<BranchPatch>,
    pub variable_references: Vec<VariableReference>,
    pub function_references: Vec<FunctionReference>,
    pub string_references: Vec<StringReference>,
}

impl VmBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn position(&self) -> Result<u32, VmError> {
        u32::try_from(self.bytes.len()).map_err(|_| VmError::CodeTooLarge)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(mut self) -> Result<Vec<u8>, VmError> {
        self.resolve_branches()?;
        Ok(self.bytes)
    }

    pub fn finish(mut self) -> Result<VmBytecode, VmError> {
        self.resolve_branches()?;
        Ok(VmBytecode {
            bytes: self.bytes,
            variable_references: self.variable_references,
            function_references: self.function_references,
            string_references: self.string_references,
        })
    }

    pub fn label(&mut self) -> Label {
        let label = Label(self.labels.len());
        self.labels.push(None);
        label
    }

    pub fn mark(&mut self, label: Label) -> Result<(), VmError> {
        let position = self.position()?;
        let address = self
            .labels
            .get_mut(label.0)
            .expect("labels can only originate from this VM buffer");
        if address.replace(position).is_some() {
            return Err(VmError::LabelAlreadyMarked);
        }
        Ok(())
    }

    pub fn emit(&mut self, opcode: Opcode, value_type: VmType) {
        self.write_u32(encode_instruction_arg(opcode, value_type as u8));
    }

    pub fn emit_types(&mut self, opcode: Opcode, first: VmType, second: VmType) {
        self.write_u32(encode_instruction_arg(opcode, encode_types(first, second)));
    }

    pub fn emit_condition(
        &mut self,
        opcode: Opcode,
        first: VmType,
        second: VmType,
        condition: Condition,
    ) {
        self.write_u32(
            encode_instruction_arg(opcode, encode_types(first, second)) | ((condition as u32) << 8),
        );
    }

    pub fn emit_dup(&mut self, value_type: VmType, count: u16) {
        debug_assert!(count != 0);
        self.write_u32(
            encode_instruction_arg(Opcode::Dup, value_type as u8) | u32::from(count - 1),
        );
    }

    pub fn emit_push_immediate(&mut self, value: i16) {
        self.write_u32(
            encode_instruction_arg(Opcode::PushImmediate, VmType::Error as u8)
                | u32::from(value as u16),
        );
    }

    /// Emits the payload-less `push.e` form synthesized by the official
    /// short-circuit and increment rewrites.
    pub fn emit_push_error(&mut self, value: u16) {
        self.write_u32(
            encode_instruction_arg(Opcode::Push, VmType::Error as u8) | u32::from(value),
        );
    }

    pub fn emit_push_i32(&mut self, value: i32) {
        self.emit(Opcode::Push, VmType::Int);
        self.write_i32(value);
    }

    pub fn emit_push_i64(&mut self, value: i64) {
        self.emit(Opcode::Push, VmType::Long);
        self.write_i64(value);
    }

    pub fn emit_push_f64(&mut self, value: f64) {
        self.emit(Opcode::Push, VmType::Double);
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn emit_push_string(&mut self, value: impl Into<String>) -> Result<(), VmError> {
        self.emit(Opcode::Push, VmType::String);
        let offset = self.position()?;
        self.string_references.push(StringReference {
            offset,
            value: value.into(),
        });
        self.write_u32(0);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn emit_variable(
        &mut self,
        opcode: Opcode,
        instance: i16,
        target_type: Option<VmType>,
        source_type: Option<VmType>,
        name: impl Into<String>,
        kind: VariableKind,
        flags: u32,
        local_slot: Option<u32>,
    ) -> Result<(), VmError> {
        let argument = match (target_type, source_type) {
            (None, None) => VmType::Variable as u8,
            (Some(target), None) => encode_types(VmType::Variable, target),
            (Some(target), Some(source)) => encode_types(source, target),
            (None, Some(_)) => unreachable!("a source VM type requires a target type"),
        };
        self.write_u32(encode_instruction_arg(opcode, argument) | u32::from(instance as u16));
        let offset = self.position()?;
        self.variable_references.push(VariableReference {
            offset,
            name: name.into(),
            kind,
            flags,
            local_slot,
        });
        self.write_u32(flags);
        Ok(())
    }

    pub fn emit_call(
        &mut self,
        name: impl Into<String>,
        argument_count: u16,
    ) -> Result<(), VmError> {
        self.write_u32(
            encode_instruction_arg(Opcode::Call, VmType::Int as u8) | u32::from(argument_count),
        );
        let offset = self.position()?;
        self.function_references.push(FunctionReference {
            offset,
            name: name.into(),
        });
        self.write_u32(0);
        Ok(())
    }

    pub fn emit_break(&mut self, value: u16) {
        self.write_u32(
            encode_instruction_arg(Opcode::Break, VmType::Error as u8) | u32::from(value),
        );
    }

    pub fn emit_break_i32(&mut self, value: u16, payload: i32) {
        self.write_u32(encode_instruction_arg(Opcode::Break, VmType::Int as u8) | u32::from(value));
        self.write_i32(payload);
    }

    pub fn emit_branch(&mut self, opcode: Opcode, label: Label) -> Result<(), VmError> {
        debug_assert!(matches!(
            opcode,
            Opcode::Branch
                | Opcode::BranchTrue
                | Opcode::BranchFalse
                | Opcode::PushEnv
                | Opcode::PopEnv
        ));
        let offset = self.position()?;
        self.branches.push(BranchPatch {
            offset,
            opcode,
            label,
        });
        self.write_u32(encode_instruction_branch(opcode, 0));
        Ok(())
    }

    pub fn resolve_branches(&mut self) -> Result<(), VmError> {
        for patch in &self.branches {
            let target = self
                .labels
                .get(patch.label.0)
                .copied()
                .flatten()
                .ok_or(VmError::UnmarkedLabel)?;
            let delta = i64::from(target) - i64::from(patch.offset);
            if delta & 3 != 0 {
                return Err(VmError::BranchNotAligned {
                    from: patch.offset,
                    to: target,
                });
            }
            let words = delta / 4;
            if !(-0x40_0000..=0x3f_ffff).contains(&words) {
                return Err(VmError::BranchOutOfRange {
                    from: patch.offset,
                    to: target,
                });
            }
            let encoded = encode_instruction_branch(patch.opcode, delta as i32);
            self.bytes[patch.offset as usize..patch.offset as usize + 4]
                .copy_from_slice(&encoded.to_le_bytes());
        }
        Ok(())
    }

    fn write_i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn write_i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
}

const fn encode_types(first: VmType, second: VmType) -> u8 {
    first as u8 | ((second as u8) << 4)
}

const fn encode_instruction_arg(opcode: Opcode, argument: u8) -> u32 {
    ((opcode as u32) << 24) | ((argument as u32) << 16)
}

const fn encode_instruction_branch(opcode: Opcode, byte_offset: i32) -> u32 {
    ((opcode as u32) << 24) | (((byte_offset >> 2) as u32) & 0x7f_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_official_instruction_encoding() {
        let mut vm = VmBuffer::new();
        vm.emit_push_immediate(16);
        vm.emit_types(Opcode::Conv, VmType::Int, VmType::Variable);
        vm.emit_variable(
            Opcode::Push,
            -1,
            None,
            None,
            "x",
            VariableKind::Instance,
            0xa000_0000,
            None,
        )
        .unwrap();
        assert_eq!(
            vm.bytes(),
            &[
                0x10, 0x00, 0x0f, 0x84, 0x00, 0x00, 0x52, 0x07, 0xff, 0xff, 0x05, 0xc0, 0x00, 0x00,
                0x00, 0xa0,
            ]
        );
        assert_eq!(vm.variable_references[0].offset, 12);
    }

    #[test]
    fn branches_are_relative_to_the_instruction_word() {
        let mut vm = VmBuffer::new();
        let end = vm.label();
        vm.emit_branch(Opcode::BranchFalse, end).unwrap();
        vm.emit_push_immediate(1);
        vm.mark(end).unwrap();
        vm.resolve_branches().unwrap();
        assert_eq!(
            vm.bytes(),
            &[0x02, 0x00, 0x00, 0xb8, 0x01, 0x00, 0x0f, 0x84]
        );
    }

    #[test]
    fn reference_payloads_start_at_the_second_word() {
        let mut vm = VmBuffer::new();
        vm.emit_call("show_debug_message", 1).unwrap();
        vm.emit_push_string("hello").unwrap();
        assert_eq!(vm.function_references[0].offset, 4);
        assert_eq!(vm.string_references[0].offset, 12);
    }
}
