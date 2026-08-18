use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::assets::Assets;

use super::ast::{
    Accessor, AssignOp, BinaryOp, Expr, ExprKind, PostfixOp, Span, Stmt, StmtKind, UnaryOp,
};
use super::builtins;
use super::project::CodeKind;
use super::semantic::{
    AnalyzedUnit, NameAccess, NameBinding, NameResolution, ProjectAnalysis, ResourceType, SymbolId,
    Symbols,
};
use super::vm::{Condition, Label, Opcode, VariableKind, VmBuffer, VmBytecode, VmError, VmType};

const SIMPLE_VARIABLE_FLAGS: u32 = 0xa000_0000;
const STACK_INSTANCE_FLAGS: u32 = 0x8000_0000;
const ARRAY_DIMENSION: i32 = 32_000;

type EnumValues = HashMap<(SymbolId, usize), i64>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalVariable {
    pub slot: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCode {
    pub kind: CodeKind,
    pub name: String,
    pub vm_name: String,
    pub bytecode: VmBytecode,
    pub local_count: usize,
    pub locals: Vec<LocalVariable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmSummary {
    pub code_units: usize,
    pub bytecode_bytes: usize,
    pub variable_references: usize,
    pub function_references: usize,
    pub string_references: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledProject {
    pub codes: Vec<CompiledCode>,
    pub summary: VmSummary,
    /// Classification bits for every resolved built-in call, including calls
    /// which code generation lowers to a dedicated VM instruction.
    pub function_classifications: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenDiagnostic {
    pub kind: CodeKind,
    pub name: String,
    pub source: PathBuf,
    pub span: Span,
    pub message: String,
}

impl fmt::Display for CodegenDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{}: {} {}: {}",
            self.source.display(),
            self.span.line,
            self.span.column,
            self.kind,
            self.name,
            self.message
        )
    }
}

pub fn compile_vm(
    assets: &Assets,
    analysis: &ProjectAnalysis<'_>,
) -> Result<CompiledProject, Vec<CodegenDiagnostic>> {
    let function_classifications = analysis
        .units
        .iter()
        .flat_map(|unit| &unit.names)
        .filter(|name| {
            name.access == NameAccess::Call && name.binding == NameBinding::BuiltinFunction
        })
        .fold(0_u64, |flags, name| {
            flags | super::classifications::get(analysis.symbols.name(name.symbol))
        }) as i64;
    let enum_values = collect_enum_values(assets, analysis)?;
    let results: Vec<_> = analysis
        .units
        .par_iter()
        .map(|unit| Compiler::new(assets, &analysis.symbols, &enum_values, unit).run())
        .collect();
    let mut codes = Vec::with_capacity(results.len());
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(code) => codes.push(code),
            Err(mut unit_errors) => errors.append(&mut unit_errors),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let summary = VmSummary {
        code_units: codes.len(),
        bytecode_bytes: codes.iter().map(|code| code.bytecode.bytes.len()).sum(),
        variable_references: codes
            .iter()
            .map(|code| code.bytecode.variable_references.len())
            .sum(),
        function_references: codes
            .iter()
            .map(|code| code.bytecode.function_references.len())
            .sum(),
        string_references: codes
            .iter()
            .map(|code| code.bytecode.string_references.len())
            .sum(),
    };
    Ok(CompiledProject {
        codes,
        summary,
        function_classifications,
    })
}

fn collect_enum_values(
    assets: &Assets,
    analysis: &ProjectAnalysis<'_>,
) -> Result<EnumValues, Vec<CodegenDiagnostic>> {
    let mut values = EnumValues::new();
    let mut errors = Vec::new();
    for unit in &analysis.units {
        EnumCollector::new(assets, &analysis.symbols, unit, &mut values, &mut errors)
            .statements(&unit.program.statements);
    }
    if errors.is_empty() {
        Ok(values)
    } else {
        Err(errors)
    }
}

struct EnumCollector<'a, 'source> {
    assets: &'a Assets,
    symbols: &'a Symbols,
    unit: &'a AnalyzedUnit<'source>,
    names: HashMap<u64, NameResolution>,
    values: &'a mut EnumValues,
    errors: &'a mut Vec<CodegenDiagnostic>,
}

impl<'a, 'source> EnumCollector<'a, 'source> {
    fn new(
        assets: &'a Assets,
        symbols: &'a Symbols,
        unit: &'a AnalyzedUnit<'source>,
        values: &'a mut EnumValues,
        errors: &'a mut Vec<CodegenDiagnostic>,
    ) -> Self {
        let names = unit
            .names
            .iter()
            .copied()
            .map(|resolution| (span_key(resolution.span), resolution))
            .collect();
        Self {
            assets,
            symbols,
            unit,
            names,
            values,
            errors,
        }
    }

    fn statements(&mut self, statements: &[Stmt]) {
        for statement in statements {
            self.statement(statement);
        }
    }

    fn statement(&mut self, statement: &Stmt) {
        match &statement.kind {
            StmtKind::Block(statements) => self.statements(statements),
            StmtKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.statement(then_branch);
                if let Some(branch) = else_branch {
                    self.statement(branch);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::DoUntil { body, .. }
            | StmtKind::Repeat { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::Switch { body, .. } => self.statement(body),
            StmtKind::For {
                initializer, body, ..
            } => {
                if let Some(initializer) = initializer {
                    self.statement(initializer);
                }
                self.statement(body);
            }
            StmtKind::Enum { name, members } => {
                let Some(resolution) = self.names.get(&span_key(*name)).copied() else {
                    self.error(*name, "missing semantic resolution for enum declaration");
                    return;
                };
                let enum_symbol = resolution.symbol;
                let mut next_value = 0_i64;
                for member in members {
                    if let Some(expression) = &member.value {
                        match self.constant(expression) {
                            Some(value) => next_value = value,
                            None => {
                                self.error(
                                    expression.span,
                                    "enum expression must be an integer constant",
                                );
                                continue;
                            }
                        }
                    }
                    let Some(member_resolution) = self.names.get(&span_key(member.name)).copied()
                    else {
                        self.error(member.name, "missing semantic resolution for enum member");
                        continue;
                    };
                    let NameBinding::EnumMember { member_index, .. } = member_resolution.binding
                    else {
                        self.error(member.name, "enum member has an invalid semantic binding");
                        continue;
                    };
                    self.values.insert((enum_symbol, member_index), next_value);
                    next_value = next_value.wrapping_add(1);
                }
            }
            _ => {}
        }
    }

    fn constant(&self, expression: &Expr) -> Option<i64> {
        match &expression.kind {
            ExprKind::Number => parse_integer(self.text(expression.span)),
            ExprKind::Group(value) => self.constant(value),
            ExprKind::Identifier => {
                let resolution = self.names.get(&span_key(expression.span))?;
                self.binding_constant(*resolution)
            }
            ExprKind::Member { name, .. } => {
                let resolution = self.names.get(&span_key(*name))?;
                self.binding_constant(*resolution)
            }
            ExprKind::Unary { op, value } => {
                let value = self.constant(value)?;
                match op {
                    UnaryOp::Positive => Some(value),
                    UnaryOp::Negative => Some(value.wrapping_neg()),
                    UnaryOp::Not => Some(i64::from(value == 0)),
                    UnaryOp::BitNot => Some(!value),
                    UnaryOp::PreIncrement | UnaryOp::PreDecrement => None,
                }
            }
            ExprKind::Binary { op, left, right } => {
                let left = self.constant(left)?;
                let right = self.constant(right)?;
                enum_binary(*op, left, right)
            }
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                if self.constant(condition)? != 0 {
                    self.constant(then_value)
                } else {
                    self.constant(else_value)
                }
            }
            _ => None,
        }
    }

    fn binding_constant(&self, resolution: NameResolution) -> Option<i64> {
        match resolution.binding {
            NameBinding::BuiltinConstant => integral(builtins::constant_value(
                self.symbols.name(resolution.symbol),
            )?),
            NameBinding::ConfiguredConstant { index } => {
                let text = self.assets.settings.constants[index].value.trim();
                parse_integer(text).or_else(|| integral(builtins::constant_value(text)?))
            }
            NameBinding::Resource { index, .. } | NameBinding::Script { index } => {
                i64::try_from(index).ok()
            }
            NameBinding::RoomInstance { id } => Some(i64::from(id)),
            NameBinding::Enum { index } => i64::try_from(index).ok(),
            NameBinding::EnumMember {
                enum_symbol,
                member_index,
            } => self.values.get(&(enum_symbol, member_index)).copied(),
            _ => None,
        }
    }

    fn text(&self, span: Span) -> &str {
        &self.unit.code[span.start as usize..span.end as usize]
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(CodegenDiagnostic {
            kind: self.unit.kind,
            name: self.unit.name.clone(),
            source: self.unit.source.to_path_buf(),
            span,
            message: message.into(),
        });
    }
}

fn parse_integer(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('$') {
        return i64::from_str_radix(hex, 16).ok();
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).ok();
    }
    text.parse::<i64>()
        .ok()
        .or_else(|| integral(text.parse::<f64>().ok()?))
}

fn integral(value: f64) -> Option<i64> {
    (value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value <= i64::MAX as f64)
        .then_some(value as i64)
}

fn enum_binary(op: BinaryOp, left: i64, right: i64) -> Option<i64> {
    let boolean = |value| Some(i64::from(value));
    match op {
        BinaryOp::LogicalOr => boolean(left != 0 || right != 0),
        BinaryOp::LogicalAnd => boolean(left != 0 && right != 0),
        BinaryOp::LogicalXor => boolean((left != 0) != (right != 0)),
        BinaryOp::Equal => boolean(left == right),
        BinaryOp::NotEqual => boolean(left != right),
        BinaryOp::Less => boolean(left < right),
        BinaryOp::LessEqual => boolean(left <= right),
        BinaryOp::Greater => boolean(left > right),
        BinaryOp::GreaterEqual => boolean(left >= right),
        BinaryOp::BitOr => Some(left | right),
        BinaryOp::BitAnd => Some(left & right),
        BinaryOp::BitXor => Some(left ^ right),
        BinaryOp::ShiftLeft => Some(left.wrapping_shl((right & 63) as u32)),
        BinaryOp::ShiftRight => Some(left.wrapping_shr((right & 63) as u32)),
        BinaryOp::Add => Some(left.wrapping_add(right)),
        BinaryOp::Subtract => Some(left.wrapping_sub(right)),
        BinaryOp::Multiply => Some(left.wrapping_mul(right)),
        BinaryOp::Divide => (right != 0).then_some((left as f64 / right as f64) as i64),
        BinaryOp::IntegerDivide => left.checked_div(right),
        BinaryOp::Modulo => left.checked_rem(right),
    }
}

#[derive(Debug, Clone, Copy)]
enum Cleanup {
    Switch(VmType),
    With,
}

#[derive(Debug, Clone, Copy)]
struct JumpTarget {
    label: Label,
    cleanup_depth: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum FoldedConstant {
    Number { value: f64, boolean: bool },
    Int64(i64),
    String(String),
}

struct Compiler<'a> {
    assets: &'a Assets,
    symbols: &'a Symbols,
    enum_values: &'a EnumValues,
    unit: &'a AnalyzedUnit<'a>,
    names: HashMap<u64, NameResolution>,
    vm: VmBuffer,
    locals: Vec<LocalVariable>,
    local_slots: HashMap<u32, u32>,
    cleanup: Vec<Cleanup>,
    break_targets: Vec<JumpTarget>,
    continue_targets: Vec<JumpTarget>,
    continue_count: u32,
    errors: Vec<CodegenDiagnostic>,
}

impl<'a> Compiler<'a> {
    fn new(
        assets: &'a Assets,
        symbols: &'a Symbols,
        enum_values: &'a EnumValues,
        unit: &'a AnalyzedUnit<'a>,
    ) -> Self {
        let names = unit
            .names
            .iter()
            .copied()
            .map(|resolution| (span_key(resolution.span), resolution))
            .collect();
        let mut locals = Vec::with_capacity(unit.locals.len() + 1);
        locals.push(LocalVariable {
            slot: 0,
            name: "arguments".to_owned(),
        });
        Self {
            assets,
            symbols,
            enum_values,
            unit,
            names,
            vm: VmBuffer::new(),
            locals,
            local_slots: HashMap::new(),
            cleanup: Vec::new(),
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            continue_count: 0,
            errors: Vec::new(),
        }
    }

    fn run(mut self) -> Result<CompiledCode, Vec<CodegenDiagnostic>> {
        for statement in &self.unit.program.statements {
            self.statement(statement);
        }
        if !self.errors.is_empty() {
            return Err(self.errors);
        }
        let mut bytecode = match std::mem::take(&mut self.vm).finish() {
            Ok(bytecode) => bytecode,
            Err(error) => {
                self.error(Span::default(), error.to_string());
                return Err(self.errors);
            }
        };
        self.finalize_locals(&mut bytecode);
        let temp_name = "$$$$temp$$$$";
        let temp_declared = self
            .unit
            .locals
            .iter()
            .any(|symbol| self.symbols.name(*symbol) == temp_name);
        let temp_generated = self
            .locals
            .iter()
            .any(|local| local.name == temp_name && !temp_declared);
        let local_count = self.unit.locals.len() + 1 + usize::from(temp_generated);
        Ok(CompiledCode {
            kind: self.unit.kind,
            name: self.unit.name.clone(),
            vm_name: self.unit.vm_name.clone(),
            bytecode,
            local_count,
            locals: self.locals,
        })
    }

    fn finalize_locals(&mut self, bytecode: &mut VmBytecode) {
        let used = bytecode
            .variable_references
            .iter()
            .filter(|reference| reference.kind == VariableKind::Local)
            .map(|reference| reference.name.as_str())
            .collect::<HashSet<_>>();

        let mut slots = HashMap::<String, u32>::new();
        let mut locals = vec![LocalVariable {
            slot: 0,
            name: "arguments".to_owned(),
        }];
        slots.insert("arguments".to_owned(), 0);

        // The official writer groups patches by variable name. A name enters
        // that dictionary at its first emitted reference, even when that
        // reference is an instance member and a later one is local. Local
        // slots therefore follow first variable-name occurrence, restricted
        // to names that actually have a local patch.
        for reference in &bytecode.variable_references {
            if !used.contains(reference.name.as_str()) || slots.contains_key(&reference.name) {
                continue;
            }
            let slot = locals.len() as u32;
            slots.insert(reference.name.clone(), slot);
            locals.push(LocalVariable {
                slot,
                name: reference.name.clone(),
            });
        }

        for reference in &mut bytecode.variable_references {
            if reference.kind == VariableKind::Local {
                reference.local_slot = slots.get(&reference.name).copied();
            }
        }
        self.locals = locals;
    }

    fn statement(&mut self, statement: &Stmt) {
        match &statement.kind {
            StmtKind::Empty | StmtKind::Enum { .. } => {}
            StmtKind::Block(statements) => {
                for statement in statements {
                    self.statement(statement);
                }
            }
            StmtKind::Var { declarations, .. } => {
                for declaration in declarations {
                    if let Some(value) = &declaration.value {
                        let value_type = self.expression(value);
                        self.pop_variable(declaration.name, value_type);
                    }
                }
            }
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.if_statement(condition, then_branch, else_branch.as_deref()),
            StmtKind::While { condition, body } => self.while_statement(condition, body),
            StmtKind::DoUntil { body, condition } => self.do_until_statement(body, condition),
            StmtKind::For {
                initializer,
                condition,
                step,
                body,
            } => self.for_statement(
                initializer.as_deref(),
                condition.as_ref(),
                step.as_ref(),
                body,
            ),
            StmtKind::Repeat { count, body } => self.repeat_statement(count, body),
            StmtKind::With { target, body } => self.with_statement(target, body),
            StmtKind::Switch { value, body } => self.switch_statement(value, body, statement.span),
            StmtKind::Case(_) | StmtKind::Default => {
                self.error(statement.span, "case/default used outside a switch block")
            }
            StmtKind::Return(value) => self.return_statement(value.as_ref(), statement.span),
            StmtKind::Exit => self.exit_statement(),
            StmtKind::Break => self.break_statement(statement.span),
            StmtKind::Continue => self.continue_statement(statement.span),
            StmtKind::Expr(expression) => self.expression_statement(expression),
        }
    }

    fn expression_statement(&mut self, expression: &Expr) {
        match &expression.kind {
            ExprKind::Assign { op, target, value } => {
                self.assignment(*op, target, value, false);
            }
            ExprKind::Unary { op, value }
                if matches!(op, UnaryOp::PreIncrement | UnaryOp::PreDecrement) =>
            {
                self.increment(value, *op == UnaryOp::PreIncrement, true, false);
            }
            ExprKind::Postfix { op, target } => {
                self.increment(target, *op == PostfixOp::Increment, false, false);
            }
            _ => {
                let value_type = self.expression(expression);
                self.vm.emit(Opcode::PopNull, value_type);
            }
        }
    }

    fn if_statement(&mut self, condition: &Expr, then_branch: &Stmt, else_branch: Option<&Stmt>) {
        if let Some(FoldedConstant::Number { value, .. }) = self.fold_constant(condition) {
            if value > 0.5 {
                self.statement(then_branch);
            } else if let Some(branch) = else_branch {
                self.statement(branch);
            }
            return;
        }
        let else_label = self.vm.label();
        let end = self.vm.label();
        let condition_type = self.expression(condition);
        self.convert(condition_type, VmType::Bool);
        self.branch(Opcode::BranchFalse, else_label, condition.span);
        self.statement(then_branch);
        if else_branch.is_some() {
            self.branch(Opcode::Branch, end, then_branch.span);
        }
        self.mark(else_label, condition.span);
        if let Some(branch) = else_branch {
            self.statement(branch);
        }
        self.mark(end, then_branch.span);
    }

    fn while_statement(&mut self, condition: &Expr, body: &Stmt) {
        let start = self.vm.label();
        let end = self.vm.label();
        let depth = self.cleanup.len();
        self.break_targets.push(JumpTarget {
            label: end,
            cleanup_depth: depth,
        });
        self.continue_targets.push(JumpTarget {
            label: start,
            cleanup_depth: depth,
        });
        self.mark(start, condition.span);
        let condition_type = self.expression(condition);
        self.convert(condition_type, VmType::Bool);
        self.branch(Opcode::BranchFalse, end, condition.span);
        self.statement(body);
        self.branch(Opcode::Branch, start, body.span);
        self.mark(end, body.span);
        self.continue_targets.pop();
        self.break_targets.pop();
    }

    fn do_until_statement(&mut self, body: &Stmt, condition: &Expr) {
        let start = self.vm.label();
        let repeat = self.vm.label();
        let end = self.vm.label();
        let depth = self.cleanup.len();
        self.break_targets.push(JumpTarget {
            label: end,
            cleanup_depth: depth,
        });
        self.continue_targets.push(JumpTarget {
            label: repeat,
            cleanup_depth: depth,
        });
        self.mark(start, body.span);
        self.statement(body);
        self.mark(repeat, condition.span);
        let condition_type = self.expression(condition);
        self.convert(condition_type, VmType::Bool);
        self.branch(Opcode::BranchFalse, start, condition.span);
        self.mark(end, condition.span);
        self.continue_targets.pop();
        self.break_targets.pop();
    }

    fn for_statement(
        &mut self,
        initializer: Option<&Stmt>,
        condition: Option<&Expr>,
        step: Option<&Expr>,
        body: &Stmt,
    ) {
        if let Some(initializer) = initializer {
            self.statement(initializer);
        }
        let start = self.vm.label();
        let repeat = self.vm.label();
        let end = self.vm.label();
        let depth = self.cleanup.len();
        self.break_targets.push(JumpTarget {
            label: end,
            cleanup_depth: depth,
        });
        self.continue_targets.push(JumpTarget {
            label: repeat,
            cleanup_depth: depth,
        });
        self.mark(start, body.span);
        if let Some(condition) = condition {
            let condition_type = self.expression(condition);
            self.convert(condition_type, VmType::Bool);
            self.branch(Opcode::BranchFalse, end, condition.span);
        }
        self.statement(body);
        self.mark(repeat, body.span);
        if let Some(step) = step {
            self.expression_statement(step);
        }
        self.branch(Opcode::Branch, start, body.span);
        self.mark(end, body.span);
        self.continue_targets.pop();
        self.break_targets.pop();
    }

    fn repeat_statement(&mut self, count: &Expr, body: &Stmt) {
        let start = self.vm.label();
        let repeat = self.vm.label();
        let end = self.vm.label();
        let count_type = self.expression(count);
        self.convert(count_type, VmType::Int);
        self.vm.emit_dup(VmType::Int, 1);
        self.vm.emit_push_i32(0);
        self.vm
            .emit_condition(Opcode::Set, VmType::Int, VmType::Int, Condition::LessEqual);
        self.branch(Opcode::BranchTrue, end, count.span);
        let depth = self.cleanup.len();
        self.break_targets.push(JumpTarget {
            label: end,
            cleanup_depth: depth,
        });
        self.continue_targets.push(JumpTarget {
            label: repeat,
            cleanup_depth: depth,
        });
        self.mark(start, body.span);
        self.statement(body);
        self.mark(repeat, body.span);
        self.vm.emit_push_i32(1);
        self.vm.emit_types(Opcode::Sub, VmType::Int, VmType::Int);
        self.vm.emit_dup(VmType::Int, 1);
        self.vm.emit_types(Opcode::Conv, VmType::Int, VmType::Bool);
        self.branch(Opcode::BranchTrue, start, body.span);
        self.mark(end, body.span);
        self.vm.emit(Opcode::PopNull, VmType::Int);
        self.continue_targets.pop();
        self.break_targets.pop();
    }

    fn with_statement(&mut self, target: &Expr, body: &Stmt) {
        let start = self.vm.label();
        let pop_env = self.vm.label();
        let end = self.vm.label();
        let direct_target = ungroup(target);
        let target_type = if matches!(direct_target.kind, ExprKind::Identifier)
            && matches!(self.text(direct_target.span), "self" | "other")
        {
            // The 1.4 compiler turns `with (self/other)` into a read of that
            // instance's built-in `id`, then feeds the resulting ID to PUSHENV.
            let instance = if self.text(direct_target.span) == "self" {
                -1
            } else {
                -2
            };
            let spec = VariableSpec {
                name: "id".to_owned(),
                kind: VariableKind::Builtin,
                local_slot: None,
                implicit_array: false,
                address: VariableAddress::Simple {
                    instance,
                    opcode: Opcode::Push,
                },
            };
            self.emit_spec_push(&spec, false);
            VmType::Variable
        } else {
            self.expression(target)
        };
        self.convert(target_type, VmType::Int);
        self.branch(Opcode::PushEnv, pop_env, target.span);
        self.mark(start, body.span);
        let outer_depth = self.cleanup.len();
        self.cleanup.push(Cleanup::With);
        self.break_targets.push(JumpTarget {
            label: end,
            cleanup_depth: outer_depth,
        });
        self.continue_targets.push(JumpTarget {
            label: pop_env,
            cleanup_depth: outer_depth + 1,
        });
        self.statement(body);
        self.continue_targets.pop();
        self.break_targets.pop();
        self.cleanup.pop();
        self.mark(pop_env, body.span);
        self.branch(Opcode::PopEnv, start, body.span);
        self.mark(end, body.span);
    }

    fn switch_statement(&mut self, value: &Expr, body: &Stmt, span: Span) {
        let StmtKind::Block(statements) = &body.kind else {
            self.error(body.span, "switch body must be a block");
            return;
        };
        let value_type = self.expression(value);
        let mut labels = Vec::new();
        let mut default = None;
        for statement in statements {
            match &statement.kind {
                StmtKind::Case(case_value) => {
                    let label = self.vm.label();
                    labels.push((statement, label));
                    self.vm.emit_dup(value_type, 1);
                    let case_type = self.expression(case_value);
                    self.vm
                        .emit_condition(Opcode::Set, case_type, value_type, Condition::Equal);
                    self.branch(Opcode::BranchTrue, label, case_value.span);
                }
                StmtKind::Default => {
                    let label = self.vm.label();
                    labels.push((statement, label));
                    default = Some(label);
                }
                _ => {}
            }
        }
        let end = self.vm.label();
        if let Some(default) = default {
            // The 1.4 compiler emits the default jump and then retains the
            // normal no-match jump as an unreachable second branch.
            self.branch(Opcode::Branch, default, span);
        }
        self.branch(Opcode::Branch, end, span);
        self.cleanup.push(Cleanup::Switch(value_type));
        self.break_targets.push(JumpTarget {
            label: end,
            cleanup_depth: self.cleanup.len(),
        });
        let outer_continue = self.continue_targets.last().copied();
        let continue_cleanup = outer_continue.map(|_| self.vm.label());
        let continue_count = self.continue_count;
        if let Some(label) = continue_cleanup {
            self.continue_targets.push(JumpTarget {
                label,
                cleanup_depth: self.cleanup.len(),
            });
        }
        let mut next_label = 0;
        for statement in statements {
            if matches!(statement.kind, StmtKind::Case(_) | StmtKind::Default) {
                self.mark(labels[next_label].1, statement.span);
                next_label += 1;
            } else {
                self.statement(statement);
            }
        }
        if continue_cleanup.is_some() {
            self.continue_targets.pop();
        }
        self.break_targets.pop();
        if let (Some(cleanup), Some(outer)) = (continue_cleanup, outer_continue)
            && self.continue_count != continue_count
        {
            self.branch(Opcode::Branch, end, span);
            self.mark(cleanup, span);
            self.vm.emit(Opcode::PopNull, value_type);
            self.branch(Opcode::Branch, outer.label, span);
        }
        self.cleanup.pop();
        self.mark(end, span);
        self.vm.emit(Opcode::PopNull, value_type);
    }

    fn break_statement(&mut self, span: Span) {
        let Some(target) = self.break_targets.last().copied() else {
            self.error(span, "break used without a loop, switch, or with context");
            return;
        };
        self.emit_cleanup(target.cleanup_depth);
        self.branch(Opcode::Branch, target.label, span);
    }

    fn continue_statement(&mut self, span: Span) {
        let Some(target) = self.continue_targets.last().copied() else {
            self.error(span, "continue used without a loop or with context");
            return;
        };
        self.continue_count = self.continue_count.wrapping_add(1);
        self.emit_cleanup(target.cleanup_depth);
        self.branch(Opcode::Branch, target.label, span);
    }

    fn return_statement(&mut self, value: Option<&Expr>, span: Span) {
        let Some(value) = value else {
            self.exit_statement();
            return;
        };
        let value_type = self.expression(value);
        self.convert(value_type, VmType::Variable);
        if self.cleanup.is_empty() {
            self.vm.emit(Opcode::Return, VmType::Variable);
            return;
        }
        let slot = self.temp_local();
        self.emit_simple_pop(
            "$$$$temp$$$$",
            VariableKind::Local,
            -7,
            slot,
            VmType::Variable,
        );
        self.emit_cleanup(0);
        self.emit_simple_push("$$$$temp$$$$", VariableKind::Local, -7, slot, false);
        self.vm.emit(Opcode::Return, VmType::Variable);
        let _ = span;
    }

    fn exit_statement(&mut self) {
        self.emit_cleanup(0);
        self.vm.emit(Opcode::Exit, VmType::Int);
    }

    fn emit_cleanup(&mut self, depth: usize) {
        for cleanup in self.cleanup[depth..].iter().rev() {
            match cleanup {
                Cleanup::Switch(value_type) => self.vm.emit(Opcode::PopNull, *value_type),
                Cleanup::With => {
                    self.vm
                        .emit_types(Opcode::PopEnv, VmType::Double, VmType::Error);
                }
            }
        }
    }

    fn expression(&mut self, expression: &Expr) -> VmType {
        if let Some(value) = self.fold_constant(expression) {
            return self.push_folded(value, expression.span);
        }
        match &expression.kind {
            ExprKind::Identifier => self.identifier(expression),
            ExprKind::Number => self.number(expression),
            ExprKind::String => self.string(expression),
            ExprKind::Group(value) => self.expression(value),
            ExprKind::Array(_) => {
                self.error(
                    expression.span,
                    "GMS 1.4 array literals are not supported by this VM pass",
                );
                self.vm.emit_push_immediate(0);
                VmType::Int
            }
            ExprKind::Unary { op, value } => self.unary(*op, value),
            ExprKind::Binary { op, left, right } => self.binary(*op, left, right),
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            } => self.conditional(condition, then_value, else_value),
            ExprKind::Assign { op, target, value } => self.assignment(*op, target, value, true),
            ExprKind::Call { callee, arguments } => self.call(callee, arguments, expression.span),
            ExprKind::Index {
                target,
                accessor,
                indices,
            } => self.index(target, *accessor, indices, expression.span),
            ExprKind::Member { name, .. } => {
                let resolution = self.resolution(*name);
                if let Some(NameResolution {
                    binding:
                        NameBinding::EnumMember {
                            enum_symbol,
                            member_index,
                        },
                    ..
                }) = resolution
                {
                    self.push_enum(enum_symbol, member_index, expression.span)
                } else {
                    self.variable_read(expression, &[])
                }
            }
            ExprKind::Postfix { op, target } => {
                self.increment(target, *op == PostfixOp::Increment, false, true)
            }
        }
    }

    fn identifier(&mut self, expression: &Expr) -> VmType {
        let Some(resolution) = self.resolution(expression.span) else {
            self.missing_resolution(expression.span);
            self.vm.emit_push_immediate(0);
            return VmType::Int;
        };
        match resolution.binding {
            NameBinding::LocalVariable { .. }
            | NameBinding::GlobalVariable
            | NameBinding::InstanceVariable
            | NameBinding::BuiltinVariable => self.variable_read(expression, &[]),
            NameBinding::BuiltinConstant => {
                let name = self.symbols.name(resolution.symbol);
                if matches!(name, "self" | "other") {
                    let instance = if name == "self" { -1 } else { -2 };
                    let spec = VariableSpec {
                        name: "id".to_owned(),
                        kind: VariableKind::Builtin,
                        local_slot: None,
                        implicit_array: false,
                        address: VariableAddress::Simple {
                            instance,
                            opcode: Opcode::Push,
                        },
                    };
                    self.emit_spec_push(&spec, false);
                    return VmType::Variable;
                }
                let value = builtins::constant_value(name).unwrap_or_else(|| {
                    self.error(
                        expression.span,
                        format!("missing value for built-in constant {name}"),
                    );
                    0.0
                });
                self.push_number(value, false)
            }
            NameBinding::ConfiguredConstant { index } => {
                let value = &self.assets.settings.constants[index].value;
                self.push_constant_text(value, expression.span)
            }
            NameBinding::Resource { kind, index } => {
                let value = match kind {
                    ResourceType::Object
                    | ResourceType::Sprite
                    | ResourceType::Sound
                    | ResourceType::Background
                    | ResourceType::Path
                    | ResourceType::Font
                    | ResourceType::Timeline
                    | ResourceType::Shader
                    | ResourceType::Room
                    | ResourceType::AudioGroup => index as i64,
                };
                self.push_integer(value)
            }
            NameBinding::RoomInstance { id } => self.push_integer(i64::from(id)),
            NameBinding::Script { index } => self.push_integer(index as i64),
            NameBinding::EnumMember {
                enum_symbol,
                member_index,
            } => self.push_enum(enum_symbol, member_index, expression.span),
            NameBinding::Enum { index } => self.push_integer(index as i64),
            NameBinding::BuiltinFunction | NameBinding::ExtensionFunction { .. } => {
                self.error(expression.span, "function name used without a call");
                self.vm.emit_push_immediate(0);
                VmType::Int
            }
        }
    }

    fn number(&mut self, expression: &Expr) -> VmType {
        let text = self.text(expression.span);
        if let Some(hex) = text.strip_prefix('$') {
            return match i64::from_str_radix(hex, 16) {
                Ok(value) => self.push_integer(value),
                Err(_) => {
                    self.error(
                        expression.span,
                        format!("invalid hexadecimal number {text}"),
                    );
                    self.vm.emit_push_immediate(0);
                    VmType::Int
                }
            };
        }
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            return match i64::from_str_radix(hex, 16) {
                Ok(value) => self.push_integer(value),
                Err(_) => {
                    self.error(
                        expression.span,
                        format!("invalid hexadecimal number {text}"),
                    );
                    self.vm.emit_push_immediate(0);
                    VmType::Int
                }
            };
        }
        match text.parse::<f64>() {
            Ok(value) => self.push_number(value, false),
            Err(_) => {
                self.error(expression.span, format!("invalid number {text}"));
                self.vm.emit_push_immediate(0);
                VmType::Int
            }
        }
    }

    fn string(&mut self, expression: &Expr) -> VmType {
        let text = self.text(expression.span);
        let value = text
            .strip_prefix(['\'', '"'])
            .and_then(|text| text.strip_suffix(['\'', '"']))
            .unwrap_or(text)
            .to_owned();
        if let Err(error) = self.vm.emit_push_string(value) {
            self.vm_error(expression.span, error);
        }
        VmType::String
    }

    fn unary(&mut self, op: UnaryOp, value: &Expr) -> VmType {
        if matches!(op, UnaryOp::PreIncrement | UnaryOp::PreDecrement) {
            return self.increment(value, op == UnaryOp::PreIncrement, true, true);
        }
        let mut value_type = self.expression(value);
        match op {
            UnaryOp::Positive => value_type,
            UnaryOp::Negative => {
                if value_type == VmType::Bool {
                    self.convert(value_type, VmType::Int);
                    value_type = VmType::Int;
                }
                self.vm.emit(Opcode::Neg, value_type);
                value_type
            }
            UnaryOp::Not => {
                self.convert(value_type, VmType::Bool);
                self.vm.emit(Opcode::Not, VmType::Bool);
                VmType::Bool
            }
            UnaryOp::BitNot => {
                if matches!(
                    value_type,
                    VmType::Double | VmType::Float | VmType::Variable
                ) {
                    self.convert(value_type, VmType::Int);
                    value_type = VmType::Int;
                }
                self.vm.emit(Opcode::Not, value_type);
                value_type
            }
            UnaryOp::PreIncrement | UnaryOp::PreDecrement => unreachable!(),
        }
    }

    fn binary(&mut self, op: BinaryOp, left: &Expr, right: &Expr) -> VmType {
        if op == BinaryOp::LogicalAnd || op == BinaryOp::LogicalOr {
            return self.short_circuit(op, left, right);
        }
        let mut left_type = self.expression(left);
        left_type = self.coerce_operand(op, left_type);
        let mut right_type = self.expression(right);
        right_type = self.coerce_operand(op, right_type);
        let result = wider_type(left_type, right_type);
        match op {
            BinaryOp::Equal => {
                self.vm
                    .emit_condition(Opcode::Set, right_type, left_type, Condition::Equal);
                VmType::Bool
            }
            BinaryOp::NotEqual => {
                self.vm
                    .emit_condition(Opcode::Set, right_type, left_type, Condition::NotEqual);
                VmType::Bool
            }
            BinaryOp::Less => self.compare(right_type, left_type, Condition::Less),
            BinaryOp::LessEqual => self.compare(right_type, left_type, Condition::LessEqual),
            BinaryOp::Greater => self.compare(right_type, left_type, Condition::Greater),
            BinaryOp::GreaterEqual => self.compare(right_type, left_type, Condition::GreaterEqual),
            BinaryOp::Add => self.operation(Opcode::Add, right_type, left_type, result),
            BinaryOp::Subtract => self.operation(Opcode::Sub, right_type, left_type, result),
            BinaryOp::Multiply => self.operation(Opcode::Mul, right_type, left_type, result),
            BinaryOp::Divide => self.operation(Opcode::Div, right_type, left_type, result),
            BinaryOp::IntegerDivide => self.operation(Opcode::Rem, right_type, left_type, result),
            BinaryOp::Modulo => self.operation(Opcode::Mod, right_type, left_type, result),
            BinaryOp::LogicalXor | BinaryOp::BitXor => {
                self.operation(Opcode::Xor, right_type, left_type, result)
            }
            BinaryOp::BitOr => self.operation(Opcode::Or, right_type, left_type, result),
            BinaryOp::BitAnd => self.operation(Opcode::And, right_type, left_type, result),
            BinaryOp::ShiftLeft => self.operation(Opcode::Shl, right_type, left_type, result),
            BinaryOp::ShiftRight => self.operation(Opcode::Shr, right_type, left_type, result),
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => unreachable!(),
        }
    }

    fn short_circuit(&mut self, op: BinaryOp, left: &Expr, right: &Expr) -> VmType {
        let shortcut = self.vm.label();
        let end = self.vm.label();
        let mut operands = Vec::new();
        collect_logical_operands(op, left, &mut operands);
        collect_logical_operands(op, right, &mut operands);
        let branch = if op == BinaryOp::LogicalAnd {
            Opcode::BranchFalse
        } else {
            Opcode::BranchTrue
        };
        for operand in &operands[..operands.len() - 1] {
            let value_type = self.expression(operand);
            self.convert(value_type, VmType::Bool);
            self.branch(branch, shortcut, operand.span);
        }
        let last = operands[operands.len() - 1];
        let value_type = self.expression(last);
        self.convert(value_type, VmType::Bool);
        self.branch(Opcode::Branch, end, last.span);
        self.mark(shortcut, left.span);
        self.vm
            .emit_push_error(u16::from(op == BinaryOp::LogicalOr));
        self.mark(end, last.span);
        VmType::Bool
    }

    fn conditional(&mut self, condition: &Expr, then_value: &Expr, else_value: &Expr) -> VmType {
        let else_label = self.vm.label();
        let end = self.vm.label();
        let condition_type = self.expression(condition);
        self.convert(condition_type, VmType::Bool);
        self.branch(Opcode::BranchFalse, else_label, condition.span);
        let then_type = self.expression(then_value);
        self.convert(then_type, VmType::Variable);
        self.branch(Opcode::Branch, end, then_value.span);
        self.mark(else_label, else_value.span);
        let else_type = self.expression(else_value);
        self.convert(else_type, VmType::Variable);
        self.mark(end, else_value.span);
        VmType::Variable
    }

    fn assignment(&mut self, op: AssignOp, target: &Expr, value: &Expr, keep: bool) -> VmType {
        if let ExprKind::Index {
            target: base,
            accessor,
            indices,
        } = &target.kind
            && *accessor != Accessor::Array
        {
            return self.accessor_assignment(
                *accessor,
                base,
                indices,
                op,
                value,
                keep,
                target.span,
            );
        }
        if op == AssignOp::Set {
            let value_type = self.expression(value);
            if keep {
                self.convert(value_type, VmType::Variable);
                self.vm.emit_dup(VmType::Variable, 1);
                self.variable_pop(target, &[], VmType::Variable);
                VmType::Variable
            } else {
                self.variable_pop(target, &[], value_type);
                value_type
            }
        } else {
            self.combined_assignment(target, op, value, keep)
        }
    }

    fn combined_assignment(
        &mut self,
        target: &Expr,
        op: AssignOp,
        value: &Expr,
        keep: bool,
    ) -> VmType {
        let (base, indices) = normal_lvalue(target);
        let Some(spec) = self.variable_spec(base) else {
            self.error(target.span, "assignment target is not a VM variable");
            return VmType::Variable;
        };
        if indices.is_empty()
            && !spec.implicit_array
            && matches!(spec.address, VariableAddress::Simple { .. })
        {
            self.emit_spec_push(&spec, false);
            let mut right_type = self.expression(value);
            right_type = self.coerce_assignment_operand(op, right_type);
            let opcode = assignment_opcode(op);
            self.vm.emit_types(opcode, right_type, VmType::Variable);
            if keep {
                self.vm.emit_dup(VmType::Variable, 1);
            }
            self.emit_spec_pop(&spec, VmType::Variable, false);
            return VmType::Variable;
        }
        self.prepare_stack_address(&spec, indices);
        let indexed = !indices.is_empty() || spec.implicit_array;
        self.vm.emit_dup(VmType::Int, if indexed { 2 } else { 1 });
        self.emit_stack_push(&spec, indices);
        let mut right_type = self.expression(value);
        right_type = self.coerce_assignment_operand(op, right_type);
        self.vm
            .emit_types(assignment_opcode(op), right_type, VmType::Variable);
        if keep {
            self.vm.emit_dup(VmType::Variable, 1);
        }
        self.emit_stack_pop(&spec, indices, VmType::Variable, true);
        VmType::Variable
    }

    fn call(&mut self, callee: &Expr, arguments: &[Expr], span: Span) -> VmType {
        if !matches!(callee.kind, ExprKind::Identifier) {
            self.error(
                span,
                "indirect function calls are not part of the GMS 1.4 VM backend yet",
            );
            self.vm.emit_push_immediate(0);
            return VmType::Int;
        }
        let name = self.text(callee.span).to_owned();
        if name == "ord"
            && arguments.len() == 1
            && let Some(value) = self.literal_string(&arguments[0])
        {
            return self.push_integer(value.chars().next().map_or(0, |value| value as i64));
        }
        self.emit_call_arguments(arguments);
        let count = match u16::try_from(arguments.len()) {
            Ok(count) => count,
            Err(_) => {
                self.error(span, "function call has more than 65535 arguments");
                u16::MAX
            }
        };
        if let Err(error) = self.vm.emit_call(name, count) {
            self.vm_error(span, error);
        }
        VmType::Variable
    }

    fn literal_string(&self, expression: &Expr) -> Option<&str> {
        match &expression.kind {
            ExprKind::Group(value) => self.literal_string(value),
            ExprKind::String => {
                let text =
                    &self.unit.code[expression.span.start as usize..expression.span.end as usize];
                text.strip_prefix(['\'', '"'])
                    .and_then(|text| text.strip_suffix(['\'', '"']))
            }
            _ => None,
        }
    }

    fn fold_constant(&self, expression: &Expr) -> Option<FoldedConstant> {
        match &expression.kind {
            ExprKind::Number => self.fold_number(expression.span),
            ExprKind::String => Some(FoldedConstant::String(
                self.literal_string(expression)?.to_owned(),
            )),
            ExprKind::Group(value) => self.fold_constant(value),
            ExprKind::Identifier => {
                let resolution = self.resolution(expression.span)?;
                self.fold_binding(resolution)
            }
            ExprKind::Member { name, .. } => {
                let resolution = self.resolution(*name)?;
                if matches!(resolution.binding, NameBinding::EnumMember { .. }) {
                    self.fold_binding(resolution)
                } else {
                    None
                }
            }
            ExprKind::Unary { op, value } => {
                let value = self.fold_constant(value)?;
                fold_unary(*op, value)
            }
            ExprKind::Binary { op, left, right } => {
                let left = self.fold_constant(left)?;
                let right = self.fold_constant(right)?;
                fold_binary(*op, left, right)
            }
            ExprKind::Call { callee, arguments } if matches!(callee.kind, ExprKind::Identifier) => {
                let name = self.text(callee.span);
                let arguments = arguments
                    .iter()
                    .map(|argument| self.fold_constant(argument))
                    .collect::<Option<Vec<_>>>()?;
                fold_function(name, &arguments)
            }
            _ => None,
        }
    }

    fn fold_number(&self, span: Span) -> Option<FoldedConstant> {
        let text = self.text(span);
        let value = if let Some(hex) = text.strip_prefix('$') {
            i64::from_str_radix(hex, 16).ok()? as f64
        } else if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            i64::from_str_radix(hex, 16).ok()? as f64
        } else {
            text.parse::<f64>().ok()?
        };
        Some(FoldedConstant::Number {
            value,
            boolean: false,
        })
    }

    fn fold_binding(&self, resolution: NameResolution) -> Option<FoldedConstant> {
        let number = |value, boolean| Some(FoldedConstant::Number { value, boolean });
        match resolution.binding {
            NameBinding::BuiltinConstant => {
                let name = self.symbols.name(resolution.symbol);
                if matches!(name, "self" | "other") {
                    None
                } else {
                    number(builtins::constant_value(name)?, false)
                }
            }
            NameBinding::ConfiguredConstant { index } => {
                self.fold_constant_text(&self.assets.settings.constants[index].value)
            }
            NameBinding::Resource { index, .. } | NameBinding::Script { index } => {
                number(index as f64, false)
            }
            NameBinding::RoomInstance { id } => number(f64::from(id), false),
            NameBinding::Enum { index } => number(index as f64, false),
            NameBinding::EnumMember {
                enum_symbol,
                member_index,
            } => number(
                self.enum_values
                    .get(&(enum_symbol, member_index))
                    .copied()? as f64,
                false,
            ),
            _ => None,
        }
    }

    fn fold_constant_text(&self, text: &str) -> Option<FoldedConstant> {
        let text = text.trim();
        if (text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('\'') && text.ends_with('\''))
        {
            return Some(FoldedConstant::String(text[1..text.len() - 1].to_owned()));
        }
        if let Some(hex) = text.strip_prefix('$') {
            return Some(FoldedConstant::Number {
                value: i64::from_str_radix(hex, 16).ok()? as f64,
                boolean: false,
            });
        }
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            return Some(FoldedConstant::Number {
                value: i64::from_str_radix(hex, 16).ok()? as f64,
                boolean: false,
            });
        }
        if let Ok(value) = text.parse::<f64>() {
            return Some(FoldedConstant::Number {
                value,
                boolean: false,
            });
        }
        builtins::constant_value(text).map(|value| FoldedConstant::Number {
            value,
            boolean: false,
        })
    }

    fn push_folded(&mut self, value: FoldedConstant, span: Span) -> VmType {
        match value {
            FoldedConstant::Number { value, boolean } => self.push_number(value, boolean),
            FoldedConstant::Int64(value) => {
                self.vm.emit_push_i64(value);
                VmType::Long
            }
            FoldedConstant::String(value) => {
                if let Err(error) = self.vm.emit_push_string(value) {
                    self.vm_error(span, error);
                }
                VmType::String
            }
        }
    }

    fn emit_call_arguments(&mut self, arguments: &[Expr]) {
        for argument in arguments.iter().rev() {
            let argument_type = self.expression(argument);
            self.convert(argument_type, VmType::Variable);
        }
    }

    fn index(&mut self, target: &Expr, accessor: Accessor, indices: &[Expr], span: Span) -> VmType {
        if accessor == Accessor::Array {
            return self.variable_read(target, indices);
        }
        let Some(function) = accessor_functions(accessor, indices.len()).map(|entry| entry.get)
        else {
            self.error(span, "unsupported accessor or accessor dimension count");
            self.vm.emit_push_immediate(0);
            return VmType::Int;
        };
        for index in indices.iter().rev() {
            let value_type = self.expression(index);
            self.convert(value_type, VmType::Variable);
        }
        let target_type = self.expression(target);
        self.convert(target_type, VmType::Variable);
        if let Err(error) = self.vm.emit_call(function, (indices.len() + 1) as u16) {
            self.vm_error(span, error);
        }
        VmType::Variable
    }

    #[allow(clippy::too_many_arguments)]
    fn accessor_assignment(
        &mut self,
        accessor: Accessor,
        target: &Expr,
        indices: &[Expr],
        op: AssignOp,
        value: &Expr,
        keep: bool,
        span: Span,
    ) -> VmType {
        let Some(functions) = accessor_functions(accessor, indices.len()) else {
            self.error(span, "unsupported accessor or accessor dimension count");
            return VmType::Variable;
        };
        let value_type = if op == AssignOp::Set {
            self.expression(value)
        } else {
            let current = self.index(target, accessor, indices, span);
            let mut right = self.expression(value);
            right = self.coerce_assignment_operand(op, right);
            self.vm.emit_types(assignment_opcode(op), right, current);
            wider_type(right, current)
        };
        self.convert(value_type, VmType::Variable);
        if keep {
            self.vm.emit_dup(VmType::Variable, 1);
        }
        for index in indices.iter().rev() {
            let index_type = self.expression(index);
            self.convert(index_type, VmType::Variable);
        }
        let target_type = self.expression(target);
        self.convert(target_type, VmType::Variable);
        if let Err(error) = self.vm.emit_call(functions.set, (indices.len() + 2) as u16) {
            self.vm_error(span, error);
        }
        if keep {
            self.vm.emit(Opcode::PopNull, VmType::Variable);
            VmType::Variable
        } else {
            self.vm.emit(Opcode::PopNull, VmType::Variable);
            value_type
        }
    }

    fn increment(&mut self, target: &Expr, increment: bool, prefix: bool, keep: bool) -> VmType {
        if let ExprKind::Index {
            target: base,
            accessor,
            indices,
        } = &target.kind
            && *accessor != Accessor::Array
        {
            return self.accessor_increment(
                *accessor,
                base,
                indices,
                increment,
                prefix,
                keep,
                target.span,
            );
        }
        let (base, indices) = normal_lvalue(target);
        let Some(spec) = self.variable_spec(base) else {
            self.error(target.span, "increment target is not a VM variable");
            return VmType::Variable;
        };
        if !indices.is_empty()
            || spec.implicit_array
            || !matches!(spec.address, VariableAddress::Simple { .. })
        {
            let indexed = !indices.is_empty() || spec.implicit_array;
            self.prepare_stack_address(&spec, indices);
            self.vm
                .emit_dup(if indexed { VmType::Long } else { VmType::Int }, 1);
            self.emit_stack_push(&spec, indices);
            if keep && !prefix {
                self.duplicate_stack_increment_result(indexed);
            }
            self.vm.emit_push_error(1);
            self.vm.emit_types(
                if increment { Opcode::Add } else { Opcode::Sub },
                VmType::Int,
                VmType::Variable,
            );
            if keep && prefix {
                self.duplicate_stack_increment_result(indexed);
            }
            self.emit_stack_pop(&spec, indices, VmType::Variable, true);
            return VmType::Variable;
        }
        self.emit_spec_push(&spec, false);
        if keep && !prefix {
            self.vm.emit_dup(VmType::Variable, 1);
        }
        self.vm.emit_push_error(1);
        self.vm.emit_types(
            if increment { Opcode::Add } else { Opcode::Sub },
            VmType::Int,
            VmType::Variable,
        );
        if keep && prefix {
            self.vm.emit_dup(VmType::Variable, 1);
        }
        self.emit_spec_pop(&spec, VmType::Variable, false);
        VmType::Variable
    }

    fn duplicate_stack_increment_result(&mut self, indexed: bool) {
        self.vm.emit_dup(VmType::Variable, 1);
        self.vm.emit_pop_reorder(if indexed { 5 } else { 6 });
    }

    #[allow(clippy::too_many_arguments)]
    fn accessor_increment(
        &mut self,
        accessor: Accessor,
        target: &Expr,
        indices: &[Expr],
        increment: bool,
        prefix: bool,
        keep: bool,
        span: Span,
    ) -> VmType {
        let Some(functions) = accessor_functions(accessor, indices.len()) else {
            self.error(span, "unsupported accessor increment");
            self.vm.emit_push_immediate(0);
            return VmType::Int;
        };
        let current = self.index(target, accessor, indices, span);
        self.vm.emit_push_error(1);
        self.vm.emit_types(
            if increment { Opcode::Add } else { Opcode::Sub },
            VmType::Int,
            current,
        );
        self.convert(VmType::Variable, VmType::Variable);
        for index in indices.iter().rev() {
            let index_type = self.expression(index);
            self.convert(index_type, VmType::Variable);
        }
        let target_type = self.expression(target);
        self.convert(target_type, VmType::Variable);
        let function = if prefix {
            functions.set_pre
        } else {
            functions.set_post
        };
        if let Err(error) = self.vm.emit_call(function, (indices.len() + 2) as u16) {
            self.vm_error(span, error);
        }
        if !keep {
            self.vm.emit(Opcode::PopNull, VmType::Variable);
        }
        VmType::Variable
    }

    fn variable_read(&mut self, target: &Expr, indices: &[Expr]) -> VmType {
        let (base, nested_indices) = if indices.is_empty() {
            normal_lvalue(target)
        } else {
            (target, indices)
        };
        let Some(spec) = self.variable_spec(base) else {
            self.error(target.span, "expression is not a VM variable");
            self.vm.emit_push_immediate(0);
            return VmType::Int;
        };
        if nested_indices.is_empty() && !spec.implicit_array {
            self.emit_spec_push(&spec, true);
        } else {
            self.prepare_stack_address(&spec, nested_indices);
            self.emit_stack_push(&spec, nested_indices);
        }
        VmType::Variable
    }

    fn variable_pop(&mut self, target: &Expr, indices: &[Expr], value_type: VmType) {
        let (base, nested_indices) = if indices.is_empty() {
            normal_lvalue(target)
        } else {
            (target, indices)
        };
        let Some(spec) = self.variable_spec(base) else {
            self.error(target.span, "assignment target is not a VM variable");
            return;
        };
        if nested_indices.is_empty()
            && !spec.implicit_array
            && matches!(spec.address, VariableAddress::Simple { .. })
        {
            self.emit_spec_pop(&spec, value_type, false);
        } else {
            self.prepare_stack_address(&spec, nested_indices);
            self.emit_stack_pop(&spec, nested_indices, value_type, false);
        }
    }

    fn pop_variable(&mut self, span: Span, value_type: VmType) {
        let expression = Expr {
            kind: ExprKind::Identifier,
            span,
        };
        self.variable_pop(&expression, &[], value_type);
    }

    fn variable_spec<'b>(&mut self, target: &'b Expr) -> Option<VariableSpec<'b>> {
        match &target.kind {
            ExprKind::Group(value) => self.variable_spec(value),
            ExprKind::Identifier => {
                let resolution = self.resolution(target.span)?;
                let name = self.symbols.name(resolution.symbol).to_owned();
                let (kind, instance, mut opcode, local_slot) =
                    binding_variable(resolution.binding)?;
                if kind == VariableKind::Builtin && builtins::is_instance_variable(&name) {
                    opcode = Opcode::Push;
                }
                let local_slot = local_slot.map(|slot| self.allocate_local(slot, &name) - 1);
                let implicit_array =
                    kind == VariableKind::Builtin && builtins::is_array_variable(&name);
                Some(VariableSpec {
                    name,
                    kind,
                    local_slot,
                    implicit_array,
                    address: VariableAddress::Simple { instance, opcode },
                })
            }
            ExprKind::Member {
                target: object,
                name,
            } => {
                let resolution = self.resolution(*name)?;
                let name = self.symbols.name(resolution.symbol).to_owned();
                let (kind, _instance, mut opcode, local_slot) =
                    binding_variable(resolution.binding)?;
                if kind == VariableKind::Builtin && builtins::is_instance_variable(&name) {
                    opcode = Opcode::Push;
                }
                let local_slot = local_slot.map(|slot| self.allocate_local(slot, &name) - 1);
                let implicit_array = builtins::is_array_variable(&name);
                let simple = if matches!(object.kind, ExprKind::Identifier) {
                    let keyword = match self.text(object.span) {
                        "self" => Some((-1, opcode)),
                        "other" => Some((-2, Opcode::Push)),
                        "global" => Some((-5, Opcode::PushGlobal)),
                        "local" => Some((-7, Opcode::PushLocal)),
                        _ => None,
                    };
                    keyword.or_else(|| {
                        let object = self.resolution(object.span)?;
                        match object.binding {
                            NameBinding::Resource {
                                kind: ResourceType::Object,
                                index,
                            } => i16::try_from(index).ok().map(|index| (index, Opcode::Push)),
                            _ => None,
                        }
                    })
                } else {
                    None
                };
                Some(VariableSpec {
                    name,
                    kind,
                    local_slot,
                    implicit_array,
                    address: simple.map_or(VariableAddress::Stack(object), |(instance, opcode)| {
                        VariableAddress::Simple { instance, opcode }
                    }),
                })
            }
            _ => None,
        }
    }

    fn emit_spec_push(&mut self, spec: &VariableSpec<'_>, specialized: bool) {
        if spec.implicit_array {
            self.prepare_stack_address(spec, &[]);
            self.emit_stack_push(spec, &[]);
            return;
        }
        match spec.address {
            VariableAddress::Simple { instance, opcode } => {
                let opcode = if specialized { opcode } else { Opcode::Push };
                self.emit_variable(opcode, instance, None, None, spec, SIMPLE_VARIABLE_FLAGS);
            }
            VariableAddress::Stack(target) => {
                let target_type = self.expression(target);
                self.convert(target_type, VmType::Int);
                self.emit_variable(Opcode::Push, 0, None, None, spec, STACK_INSTANCE_FLAGS);
            }
        }
    }

    fn emit_spec_pop(&mut self, spec: &VariableSpec<'_>, value_type: VmType, stack: bool) {
        if spec.implicit_array {
            self.prepare_stack_address(spec, &[]);
            self.emit_stack_pop(spec, &[], value_type, false);
            return;
        }
        match spec.address {
            VariableAddress::Simple { instance, .. } if !stack => self.emit_variable(
                Opcode::Pop,
                instance,
                Some(value_type),
                None,
                spec,
                SIMPLE_VARIABLE_FLAGS,
            ),
            _ => self.emit_stack_pop(spec, &[], value_type, false),
        }
    }

    fn prepare_stack_address(&mut self, spec: &VariableSpec<'_>, indices: &[Expr]) {
        match spec.address {
            VariableAddress::Simple { instance, .. } => {
                self.vm.emit_push_immediate(instance);
            }
            VariableAddress::Stack(target) => {
                let target_type = self.expression(target);
                self.convert(target_type, VmType::Int);
            }
        }
        if let Some(first) = indices.first() {
            let first_type = self.expression(first);
            self.convert(first_type, VmType::Int);
            if let Some(second) = indices.get(1) {
                self.vm.emit_break(u16::MAX);
                self.vm.emit_push_i32(ARRAY_DIMENSION);
                self.vm.emit_types(Opcode::Mul, VmType::Int, VmType::Int);
                let second_type = self.expression(second);
                self.convert(second_type, VmType::Int);
                self.vm.emit_break(u16::MAX);
                self.vm.emit_types(Opcode::Add, VmType::Int, VmType::Int);
            }
            if indices.len() > 2 {
                self.error(
                    indices[2].span,
                    "GMS 1.4 VM arrays support at most two dimensions",
                );
            }
        } else if spec.implicit_array {
            // Built-in array variables receive an implicit Int64 zero index
            // during the official parser rewrite.
            self.vm.emit_push_i64(0);
            self.convert(VmType::Long, VmType::Int);
        }
    }

    fn emit_stack_push(&mut self, spec: &VariableSpec<'_>, indices: &[Expr]) {
        self.emit_variable(
            Opcode::Push,
            0,
            None,
            None,
            spec,
            if indices.is_empty() && !spec.implicit_array {
                STACK_INSTANCE_FLAGS
            } else {
                0
            },
        );
    }

    fn emit_stack_pop(
        &mut self,
        spec: &VariableSpec<'_>,
        indices: &[Expr],
        value_type: VmType,
        combined: bool,
    ) {
        self.emit_variable(
            Opcode::Pop,
            0,
            Some(if combined {
                VmType::Variable
            } else {
                value_type
            }),
            combined.then_some(VmType::Int),
            spec,
            if indices.is_empty() && !spec.implicit_array {
                STACK_INSTANCE_FLAGS
            } else {
                0
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_variable(
        &mut self,
        opcode: Opcode,
        instance: i16,
        target_type: Option<VmType>,
        source_type: Option<VmType>,
        spec: &VariableSpec<'_>,
        flags: u32,
    ) {
        if let Err(error) = self.vm.emit_variable(
            opcode,
            instance,
            target_type,
            source_type,
            spec.name.clone(),
            spec.kind,
            flags,
            spec.local_slot.map(|slot| slot + 1),
        ) {
            self.vm_error(Span::default(), error);
        }
    }

    fn emit_simple_push(
        &mut self,
        name: &str,
        kind: VariableKind,
        instance: i16,
        slot: u32,
        specialized: bool,
    ) {
        let spec = VariableSpec {
            name: name.to_owned(),
            kind,
            local_slot: Some(slot - 1),
            implicit_array: false,
            address: VariableAddress::Simple {
                instance,
                opcode: if specialized {
                    Opcode::PushLocal
                } else {
                    Opcode::Push
                },
            },
        };
        self.emit_spec_push(&spec, specialized);
    }

    fn emit_simple_pop(
        &mut self,
        name: &str,
        kind: VariableKind,
        instance: i16,
        slot: u32,
        value_type: VmType,
    ) {
        let spec = VariableSpec {
            name: name.to_owned(),
            kind,
            local_slot: Some(slot - 1),
            implicit_array: false,
            address: VariableAddress::Simple {
                instance,
                opcode: Opcode::PushLocal,
            },
        };
        self.emit_spec_pop(&spec, value_type, false);
    }

    fn coerce_operand(&mut self, op: BinaryOp, value_type: VmType) -> VmType {
        match op {
            BinaryOp::Divide if !matches!(value_type, VmType::Double | VmType::Variable) => {
                self.convert(value_type, VmType::Double);
                VmType::Double
            }
            BinaryOp::Add
            | BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::IntegerDivide
            | BinaryOp::Modulo
                if value_type == VmType::Bool =>
            {
                self.convert(value_type, VmType::Int);
                VmType::Int
            }
            BinaryOp::LogicalXor if value_type != VmType::Bool => {
                self.convert(value_type, VmType::Bool);
                VmType::Bool
            }
            BinaryOp::BitOr | BinaryOp::BitAnd | BinaryOp::BitXor => {
                if value_type == VmType::Int {
                    VmType::Int
                } else if matches!(value_type, VmType::Variable | VmType::Double | VmType::Long) {
                    if value_type != VmType::Long {
                        self.convert(value_type, VmType::Long);
                    }
                    VmType::Long
                } else {
                    self.convert(value_type, VmType::Int);
                    VmType::Int
                }
            }
            BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                self.convert(value_type, VmType::Long);
                VmType::Long
            }
            _ => value_type,
        }
    }

    fn coerce_assignment_operand(&mut self, op: AssignOp, value_type: VmType) -> VmType {
        if matches!(op, AssignOp::BitOr | AssignOp::BitAnd | AssignOp::BitXor) {
            if matches!(value_type, VmType::Int | VmType::Long) {
                value_type
            } else {
                self.convert(value_type, VmType::Int);
                VmType::Int
            }
        } else if value_type == VmType::Bool {
            self.convert(value_type, VmType::Int);
            VmType::Int
        } else {
            value_type
        }
    }

    fn operation(&mut self, opcode: Opcode, right: VmType, left: VmType, result: VmType) -> VmType {
        self.vm.emit_types(opcode, right, left);
        result
    }

    fn compare(&mut self, right: VmType, left: VmType, condition: Condition) -> VmType {
        self.vm.emit_condition(Opcode::Set, right, left, condition);
        VmType::Bool
    }

    fn convert(&mut self, from: VmType, to: VmType) {
        if from != to {
            self.vm.emit_types(Opcode::Conv, from, to);
        }
    }

    fn push_constant_text(&mut self, text: &str, span: Span) -> VmType {
        let text = text.trim();
        if (text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('\'') && text.ends_with('\''))
        {
            if let Err(error) = self.vm.emit_push_string(&text[1..text.len() - 1]) {
                self.vm_error(span, error);
            }
            return VmType::String;
        }
        if let Some(hex) = text.strip_prefix('$')
            && let Ok(value) = i64::from_str_radix(hex, 16)
        {
            return self.push_integer(value);
        }
        if let Ok(value) = text.parse::<f64>() {
            return self.push_number(value, false);
        }
        if let Some(value) = builtins::constant_value(text) {
            return self.push_number(value, false);
        }
        self.error(
            span,
            format!("configured constant expression {text:?} is not yet foldable"),
        );
        self.vm.emit_push_immediate(0);
        VmType::Int
    }

    fn push_number(&mut self, value: f64, boolean: bool) -> VmType {
        if boolean {
            self.vm.emit_push_immediate(value as i16);
            return VmType::Bool;
        }
        if value.is_finite()
            && value.fract() == 0.0
            && value >= i64::MIN as f64
            && value <= i64::MAX as f64
        {
            return self.push_integer(value as i64);
        }
        self.vm.emit_push_f64(value);
        VmType::Double
    }

    fn push_integer(&mut self, value: i64) -> VmType {
        if let Ok(value) = i16::try_from(value) {
            self.vm.emit_push_immediate(value);
            VmType::Int
        } else if let Ok(value) = i32::try_from(value) {
            self.vm.emit_push_i32(value);
            VmType::Int
        } else {
            self.vm.emit_push_i64(value);
            VmType::Long
        }
    }

    fn push_enum(&mut self, enum_symbol: SymbolId, member_index: usize, span: Span) -> VmType {
        let value = self
            .enum_values
            .get(&(enum_symbol, member_index))
            .copied()
            .unwrap_or_else(|| {
                self.error(span, "missing folded value for enum member");
                member_index as i64
            });
        self.push_integer(value)
    }

    fn resolution(&self, span: Span) -> Option<NameResolution> {
        self.names.get(&span_key(span)).copied()
    }

    fn text(&self, span: Span) -> &str {
        &self.unit.code[span.start as usize..span.end as usize]
    }

    fn temp_local(&mut self) -> u32 {
        if let Some(local) = self
            .locals
            .iter()
            .find(|local| local.name == "$$$$temp$$$$")
        {
            return local.slot;
        }
        let slot = self.locals.len() as u32;
        self.locals.push(LocalVariable {
            slot,
            name: "$$$$temp$$$$".to_owned(),
        });
        slot
    }

    fn allocate_local(&mut self, semantic_slot: u32, name: &str) -> u32 {
        if let Some(slot) = self.local_slots.get(&semantic_slot) {
            return *slot;
        }
        let slot = self.locals.len() as u32;
        self.locals.push(LocalVariable {
            slot,
            name: name.to_owned(),
        });
        self.local_slots.insert(semantic_slot, slot);
        slot
    }

    fn branch(&mut self, opcode: Opcode, label: Label, span: Span) {
        if let Err(error) = self.vm.emit_branch(opcode, label) {
            self.vm_error(span, error);
        }
    }

    fn mark(&mut self, label: Label, span: Span) {
        if let Err(error) = self.vm.mark(label) {
            self.vm_error(span, error);
        }
    }

    fn missing_resolution(&mut self, span: Span) {
        self.error(
            span,
            format!("missing semantic resolution for {}", self.text(span)),
        );
    }

    fn vm_error(&mut self, span: Span, error: VmError) {
        self.error(span, error.to_string());
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(CodegenDiagnostic {
            kind: self.unit.kind,
            name: self.unit.name.clone(),
            source: self.unit.source.to_path_buf(),
            span,
            message: message.into(),
        });
    }
}

#[derive(Clone, Copy)]
enum VariableAddress<'a> {
    Simple { instance: i16, opcode: Opcode },
    Stack(&'a Expr),
}

struct VariableSpec<'a> {
    name: String,
    kind: VariableKind,
    local_slot: Option<u32>,
    implicit_array: bool,
    address: VariableAddress<'a>,
}

#[derive(Clone, Copy)]
struct AccessorFunctions {
    get: &'static str,
    set: &'static str,
    set_pre: &'static str,
    set_post: &'static str,
}

fn accessor_functions(accessor: Accessor, dimensions: usize) -> Option<AccessorFunctions> {
    let functions = match (accessor, dimensions) {
        (Accessor::Map, 1) => (
            "ds_map_find_value",
            "ds_map_set",
            "ds_map_set_pre",
            "ds_map_set_post",
        ),
        (Accessor::List, 1) => (
            "ds_list_find_value",
            "ds_list_set",
            "ds_list_set_pre",
            "ds_list_set_post",
        ),
        (Accessor::Grid, 2) => (
            "ds_grid_get",
            "ds_grid_set",
            "ds_grid_set_pre",
            "ds_grid_set_post",
        ),
        (Accessor::ArrayDirect, 1) => ("array_get", "array_set", "array_set_pre", "array_set_post"),
        (Accessor::ArrayDirect, 2) => (
            "array_get_2D",
            "array_set_2D",
            "array_set_2D_pre",
            "array_set_2D_post",
        ),
        _ => return None,
    };
    Some(AccessorFunctions {
        get: functions.0,
        set: functions.1,
        set_pre: functions.2,
        set_post: functions.3,
    })
}

fn normal_lvalue(expression: &Expr) -> (&Expr, &[Expr]) {
    if let ExprKind::Index {
        target,
        accessor: Accessor::Array,
        indices,
    } = &expression.kind
    {
        (target, indices)
    } else {
        (expression, &[])
    }
}

fn ungroup(mut expression: &Expr) -> &Expr {
    while let ExprKind::Group(value) = &expression.kind {
        expression = value;
    }
    expression
}

fn collect_logical_operands<'a>(op: BinaryOp, expression: &'a Expr, output: &mut Vec<&'a Expr>) {
    if let ExprKind::Binary {
        op: nested_op,
        left,
        right,
    } = &expression.kind
        && *nested_op == op
    {
        collect_logical_operands(op, left, output);
        collect_logical_operands(op, right, output);
    } else {
        output.push(expression);
    }
}

fn fold_unary(op: UnaryOp, value: FoldedConstant) -> Option<FoldedConstant> {
    match (op, value) {
        (UnaryOp::Positive, value) => Some(value),
        (UnaryOp::Negative, FoldedConstant::Number { value, boolean }) => {
            Some(FoldedConstant::Number {
                value: -value,
                boolean,
            })
        }
        (UnaryOp::Negative, FoldedConstant::Int64(value)) => {
            Some(FoldedConstant::Int64(value.wrapping_neg()))
        }
        (UnaryOp::Not, FoldedConstant::Number { value, boolean }) => Some(FoldedConstant::Number {
            value: f64::from(value < 0.5),
            boolean,
        }),
        (UnaryOp::Not, FoldedConstant::Int64(value)) => {
            Some(FoldedConstant::Int64(i64::from((value as f64) < 0.5)))
        }
        (UnaryOp::BitNot, FoldedConstant::Number { value, boolean }) => {
            Some(FoldedConstant::Number {
                value: (!float_to_i64(value)) as f64,
                boolean,
            })
        }
        (UnaryOp::BitNot, FoldedConstant::Int64(value)) => Some(FoldedConstant::Int64(!value)),
        (UnaryOp::PreIncrement | UnaryOp::PreDecrement, _) | (_, FoldedConstant::String(_)) => None,
    }
}

fn fold_binary(
    op: BinaryOp,
    left: FoldedConstant,
    right: FoldedConstant,
) -> Option<FoldedConstant> {
    use FoldedConstant::{Int64, Number, String};

    if matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        let ordering = match (&left, &right) {
            (Number { value: left, .. }, Number { value: right, .. }) => left - right,
            (Int64(left), Int64(right)) => left.wrapping_sub(*right) as f64,
            (Int64(left), Number { value: right, .. }) => {
                left.wrapping_sub(float_to_i64(*right)) as f64
            }
            (Number { value: left, .. }, Int64(right)) => {
                float_to_i64(*left).wrapping_sub(*right) as f64
            }
            (String(left), String(right)) => match left.cmp(right) {
                std::cmp::Ordering::Less => -1.0,
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Greater => 1.0,
            },
            _ => return None,
        };
        let result = match op {
            BinaryOp::Equal => ordering == 0.0,
            BinaryOp::NotEqual => ordering != 0.0,
            BinaryOp::Less => ordering < 0.0,
            BinaryOp::LessEqual => ordering <= 0.0,
            BinaryOp::Greater => ordering > 0.0,
            BinaryOp::GreaterEqual => ordering >= 0.0,
            _ => unreachable!(),
        };
        return Some(Number {
            value: f64::from(result),
            boolean: true,
        });
    }

    if matches!(
        op,
        BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::LogicalXor
    ) {
        let left_bool = folded_truth(&left)?;
        let right_bool = folded_truth(&right)?;
        let result = match op {
            BinaryOp::LogicalAnd => left_bool && right_bool,
            BinaryOp::LogicalOr => left_bool || right_bool,
            BinaryOp::LogicalXor => left_bool != right_bool,
            _ => unreachable!(),
        };
        let boolean = matches!(left, Number { boolean: true, .. });
        return Some(Number {
            value: f64::from(result),
            boolean,
        });
    }

    let left_boolean = matches!(left, Number { boolean: true, .. });
    match (op, left, right) {
        (BinaryOp::Add, String(left), String(right)) => Some(String(left + &right)),
        (BinaryOp::Multiply, Number { value: count, .. }, String(value)) => {
            let count = float_to_i64(count).max(0) as usize;
            Some(String(value.repeat(count)))
        }

        (BinaryOp::Add, Number { value: left, .. }, Number { value: right, .. }) => Some(Number {
            value: left + right,
            boolean: left_boolean,
        }),
        (BinaryOp::Subtract, Number { value: left, .. }, Number { value: right, .. }) => {
            Some(Number {
                value: left - right,
                boolean: left_boolean,
            })
        }
        (BinaryOp::Multiply, Number { value: left, .. }, Number { value: right, .. }) => {
            Some(Number {
                value: left * right,
                boolean: left_boolean,
            })
        }
        (BinaryOp::Divide, Number { value: left, .. }, Number { value: right, .. })
            if right != 0.0 =>
        {
            Some(Number {
                value: left / right,
                boolean: left_boolean,
            })
        }
        (BinaryOp::IntegerDivide, Number { value: left, .. }, Number { value: right, .. }) => {
            let left = float_to_i32(left);
            let right = float_to_i32(right);
            Some(Number {
                value: left.checked_div(right)? as f64,
                boolean: left_boolean,
            })
        }
        (BinaryOp::Modulo, Number { value: left, .. }, Number { value: right, .. })
            if float_to_i32(right) != 0 =>
        {
            Some(Number {
                value: left % right,
                boolean: left_boolean,
            })
        }

        (BinaryOp::Add, Int64(left), Int64(right)) => Some(Int64(left.wrapping_add(right))),
        (BinaryOp::Subtract, Int64(left), Int64(right)) => Some(Int64(left.wrapping_sub(right))),
        (BinaryOp::Multiply, Int64(left), Int64(right)) => Some(Int64(left.wrapping_mul(right))),
        (BinaryOp::Divide | BinaryOp::IntegerDivide, Int64(left), Int64(right)) => {
            Some(Int64(left.checked_div(right)?))
        }
        (BinaryOp::Modulo, Int64(left), Int64(right)) => Some(Int64(left.checked_rem(right)?)),

        (BinaryOp::Add, Int64(left), Number { value: right, .. }) => {
            Some(Int64(left.wrapping_add(float_to_i64(right))))
        }
        (BinaryOp::Subtract, Int64(left), Number { value: right, .. }) => {
            Some(Int64(left.wrapping_sub(float_to_i64(right))))
        }
        (BinaryOp::Multiply, Int64(left), Number { value: right, .. }) => {
            Some(Int64(left.wrapping_mul(float_to_i64(right))))
        }
        (BinaryOp::Divide | BinaryOp::IntegerDivide, Int64(left), Number { value: right, .. }) => {
            let right = float_to_i64(right);
            Some(Int64(left.checked_div(right)?))
        }
        (BinaryOp::Modulo, Int64(left), Number { value: right, .. }) => {
            let right = float_to_i64(right);
            Some(Int64(left.checked_rem(right)?))
        }

        (BinaryOp::Add, Number { value: left, .. }, Int64(right)) => {
            Some(Int64(float_to_i64(left).wrapping_add(right)))
        }
        (BinaryOp::Subtract, Number { value: left, .. }, Int64(right)) => {
            Some(Int64(float_to_i64(left).wrapping_sub(right)))
        }
        (BinaryOp::Multiply, Number { value: left, .. }, Int64(right)) => {
            Some(Int64(float_to_i64(left).wrapping_mul(right)))
        }
        (BinaryOp::Divide | BinaryOp::IntegerDivide, Number { value: left, .. }, Int64(right)) => {
            Some(Int64(float_to_i64(left).checked_div(right)?))
        }
        (BinaryOp::Modulo, Number { value: left, .. }, Int64(right)) => {
            Some(Int64(float_to_i64(left).checked_rem(right)?))
        }

        (BinaryOp::BitOr, Number { value: left, .. }, Number { value: right, .. }) => {
            Some(Number {
                value: (float_to_i64(left) | float_to_i64(right)) as f64,
                boolean: left_boolean,
            })
        }
        (BinaryOp::BitAnd, Number { value: left, .. }, Number { value: right, .. }) => {
            Some(Number {
                value: (float_to_i64(left) & float_to_i64(right)) as f64,
                boolean: left_boolean,
            })
        }
        (BinaryOp::BitXor, Number { value: left, .. }, Number { value: right, .. }) => {
            Some(Number {
                value: (float_to_i64(left) ^ float_to_i64(right)) as f64,
                boolean: left_boolean,
            })
        }
        (BinaryOp::ShiftLeft, Number { value: left, .. }, Number { value: right, .. }) => {
            Some(Number {
                value: float_to_i64(left).wrapping_shl((float_to_i32(right) & 63) as u32) as f64,
                boolean: left_boolean,
            })
        }
        (BinaryOp::ShiftRight, Number { value: left, .. }, Number { value: right, .. }) => {
            Some(Number {
                value: float_to_i64(left).wrapping_shr((float_to_i32(right) & 63) as u32) as f64,
                boolean: left_boolean,
            })
        }

        (BinaryOp::BitOr, Int64(left), Int64(right)) => Some(Int64(left | right)),
        (BinaryOp::BitAnd, Int64(left), Int64(right)) => Some(Int64(left & right)),
        (BinaryOp::BitXor, Int64(left), Int64(right)) => Some(Int64(left ^ right)),
        (BinaryOp::ShiftLeft, Int64(left), Int64(right)) => {
            Some(Int64(left.wrapping_shl((right & 63) as u32)))
        }
        (BinaryOp::ShiftRight, Int64(left), Int64(right)) => {
            Some(Int64(left.wrapping_shr((right & 63) as u32)))
        }

        (BinaryOp::BitOr, Int64(left), Number { value: right, .. }) => {
            Some(Int64(left | float_to_i64(right)))
        }
        (BinaryOp::BitAnd, Int64(left), Number { value: right, .. }) => {
            Some(Int64(left & float_to_i64(right)))
        }
        (BinaryOp::BitXor, Int64(left), Number { value: right, .. }) => {
            Some(Int64(left ^ float_to_i64(right)))
        }
        (BinaryOp::ShiftLeft, Int64(left), Number { value: right, .. }) => {
            Some(Int64(left.wrapping_shl((float_to_i32(right) & 63) as u32)))
        }
        (BinaryOp::ShiftRight, Int64(left), Number { value: right, .. }) => {
            Some(Int64(left.wrapping_shr((float_to_i32(right) & 63) as u32)))
        }

        (BinaryOp::BitOr, Number { value: left, .. }, Int64(right)) => {
            Some(Int64(float_to_i64(left) | right))
        }
        (BinaryOp::BitAnd, Number { value: left, .. }, Int64(right)) => {
            Some(Int64(float_to_i64(left) & right))
        }
        (BinaryOp::BitXor, Number { value: left, .. }, Int64(right)) => {
            Some(Int64(float_to_i64(left) ^ right))
        }
        (BinaryOp::ShiftLeft, Number { value: left, .. }, Int64(right)) => Some(Number {
            value: float_to_i32(left).wrapping_shl((right & 31) as u32) as f64,
            boolean: left_boolean,
        }),
        (BinaryOp::ShiftRight, Number { value: left, .. }, Int64(right)) => Some(Number {
            value: float_to_i32(left).wrapping_shr((right & 31) as u32) as f64,
            boolean: left_boolean,
        }),
        _ => None,
    }
}

fn fold_function(name: &str, arguments: &[FoldedConstant]) -> Option<FoldedConstant> {
    use FoldedConstant::{Int64, Number, String};
    let [argument] = arguments else {
        return None;
    };
    match (name, argument) {
        ("ord", String(value)) => Some(Number {
            value: value.encode_utf16().next().unwrap_or(0) as f64,
            boolean: false,
        }),
        ("chr", Number { value, .. }) => char::from_u32(float_to_i64(*value) as u16 as u32)
            .map(|value| String(value.to_string())),
        ("chr", Int64(value)) => {
            char::from_u32(*value as u16 as u32).map(|value| String(value.to_string()))
        }
        ("int64", Number { value, .. }) => Some(Int64(float_to_i64_rounded(*value)?)),
        ("int64", Int64(value)) => Some(Int64(*value)),
        ("real", Number { value, .. }) => Some(Number {
            value: *value,
            boolean: false,
        }),
        ("real", Int64(value)) => Some(Number {
            value: *value as f64,
            boolean: false,
        }),
        ("real", String(value)) => Some(Number {
            value: value.parse().ok()?,
            boolean: false,
        }),
        ("string", Number { value, .. }) => Some(String(value.to_string())),
        ("string", Int64(value)) => Some(String(value.to_string())),
        ("string", String(value)) => Some(String(value.clone())),
        _ => None,
    }
}

fn folded_truth(value: &FoldedConstant) -> Option<bool> {
    match value {
        FoldedConstant::Number { value, .. } => Some(*value >= 0.5),
        FoldedConstant::Int64(value) => Some((*value as f64) >= 0.5),
        FoldedConstant::String(_) => None,
    }
}

fn float_to_i32(value: f64) -> i32 {
    value as i32
}

fn float_to_i64(value: f64) -> i64 {
    value as i64
}

fn float_to_i64_rounded(value: f64) -> Option<i64> {
    value.is_finite().then(|| value.round_ties_even() as i64)
}

fn binding_variable(binding: NameBinding) -> Option<(VariableKind, i16, Opcode, Option<u32>)> {
    match binding {
        NameBinding::LocalVariable { slot } => {
            Some((VariableKind::Local, -7, Opcode::PushLocal, Some(slot)))
        }
        NameBinding::GlobalVariable => Some((VariableKind::Global, -5, Opcode::PushGlobal, None)),
        NameBinding::InstanceVariable => Some((VariableKind::Instance, -1, Opcode::Push, None)),
        NameBinding::BuiltinVariable => {
            Some((VariableKind::Builtin, -1, Opcode::PushBuiltin, None))
        }
        _ => None,
    }
}

fn assignment_opcode(op: AssignOp) -> Opcode {
    match op {
        AssignOp::Set => unreachable!(),
        AssignOp::Add => Opcode::Add,
        AssignOp::Subtract => Opcode::Sub,
        AssignOp::Multiply => Opcode::Mul,
        AssignOp::Divide => Opcode::Div,
        AssignOp::Modulo => Opcode::Mod,
        AssignOp::BitOr => Opcode::Or,
        AssignOp::BitAnd => Opcode::And,
        AssignOp::BitXor => Opcode::Xor,
    }
}

fn wider_type(first: VmType, second: VmType) -> VmType {
    let first_size = type_size(first);
    let second_size = type_size(second);
    if first_size > second_size {
        first
    } else if second_size > first_size {
        second
    } else if (first as u8) < (second as u8) {
        first
    } else {
        second
    }
}

fn type_size(value_type: VmType) -> u8 {
    match value_type {
        VmType::Double | VmType::Long => 8,
        VmType::Variable => 12,
        VmType::Float | VmType::Int | VmType::Bool | VmType::String => 4,
        _ => 0,
    }
}

const fn span_key(span: Span) -> u64 {
    ((span.start as u64) << 32) | span.end as u64
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::assets::Assets;
    use crate::config::Config;
    use crate::project::ProjectManifest;

    use super::*;
    use crate::gml::analyze_assets;

    #[test]
    fn compiles_basic_control_flow_and_relocations() {
        let compiled = compile_source(
            "vm-codegen",
            "enum E { A = 4, B } var i = E.B; repeat (3) { i += 1; } with (other) { i += ord(\"A\"); } if (i > 3) show_debug_message(\"ok\"); return i;",
        );
        assert_eq!(compiled.codes.len(), 1);
        assert_eq!(compiled.codes[0].locals[0].name, "arguments");
        assert_eq!(compiled.codes[0].locals[1].name, "i");
        assert!(compiled.summary.bytecode_bytes > 32);
        assert!(compiled.summary.variable_references >= 4);
        assert!(
            compiled.codes[0]
                .bytecode
                .variable_references
                .iter()
                .any(|reference| reference.name == "id")
        );
        assert_eq!(compiled.summary.function_references, 1);
        assert_eq!(compiled.summary.string_references, 1);
    }

    #[test]
    fn local_slots_follow_first_emitted_name_across_variable_kinds() {
        let compiled = compile_source(
            "vm-local-order",
            "holder.late = 1; var early = 2; var late = 3; return early + late;",
        );
        let code = &compiled.codes[0];
        let names = code
            .locals
            .iter()
            .map(|local| local.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["arguments", "late", "early"]);
        assert_eq!(code.local_count, 3);
        assert!(
            code.bytecode
                .variable_references
                .iter()
                .filter(|reference| reference.kind == VariableKind::Local)
                .all(|reference| reference.local_slot
                    == Some(if reference.name == "late" { 1 } else { 2 }))
        );
    }

    #[test]
    fn code_local_count_keeps_unused_and_generated_locals_separate() {
        let unused = compile_source("vm-unused-local", "var unused; return 1;");
        assert_eq!(unused.codes[0].local_count, 2);
        assert_eq!(unused.codes[0].locals.len(), 1);

        let generated = compile_source(
            "vm-generated-local",
            "switch (argument0) { case 0: return 1; default: return 2; }",
        );
        assert_eq!(generated.codes[0].local_count, 2);
        assert_eq!(generated.codes[0].locals.len(), 2);
        assert_eq!(generated.codes[0].locals[1].name, "$$$$temp$$$$");
    }

    #[test]
    fn compiles_array_and_dynamic_member_increment_statements() {
        compile_source(
            "vm-increment-statements",
            "a[0]++; --a[1]; holder.value++; --holder.value;",
        );
    }

    #[test]
    fn compiles_prefix_increment_after_semicolonless_literal_assignments() {
        compile_source(
            "vm-prefix-after-assignment",
            "a = 1\n++a\nvalues[0] = 1\n++values[0]",
        );
    }

    #[test]
    fn compiles_writable_project_macro_as_its_expanded_global() {
        let compiled = compile_source_with_constants(
            "vm-writable-macro",
            "nsfs_is_available = true; return nsfs_is_available;",
            &[("nsfs_is_available", "global.g_nsfs_is_available")],
        );
        let references = &compiled.codes[0].bytecode.variable_references;

        assert!(references.iter().any(|reference| {
            reference.name == "g_nsfs_is_available" && reference.kind == VariableKind::Global
        }));
        assert!(
            !references
                .iter()
                .any(|reference| reference.name == "nsfs_is_available")
        );
    }

    #[test]
    fn compiles_official_compiler_only_constants_as_immediates() {
        let compiled = compile_source(
            "vm-compiler-constants",
            "display_set_windows_vertex_buffer_method(vbm_fast); \
             display_set_windows_vertex_buffer_method(vbm_compatible); \
             display_set_windows_vertex_buffer_method(vbm_most_compatible); \
             vertex_format_add_custom(vertex_type_float4, vertex_usage_position); \
             gpu_set_zfunc(cmpfunc_always); gpu_set_cullmode(cull_counterclockwise); \
             return lighttype_point + path_action_reverse + tm_countvsyncs;",
        );
        let compiler_only = [
            "vbm_fast",
            "vbm_compatible",
            "vbm_most_compatible",
            "vertex_type_float4",
            "vertex_usage_position",
            "cmpfunc_always",
            "cull_counterclockwise",
            "lighttype_point",
            "path_action_reverse",
            "tm_countvsyncs",
        ];

        assert!(
            compiled.codes[0]
                .bytecode
                .variable_references
                .iter()
                .all(|reference| !compiler_only.contains(&reference.name.as_str()))
        );
    }

    #[test]
    fn preserves_prefix_and_postfix_results_for_stack_addressed_increments() {
        let cases = [
            ("vm-array-post-inc", "return a[0]++;", 5, Opcode::Add, false),
            ("vm-array-pre-dec", "return --a[0];", 5, Opcode::Sub, true),
            (
                "vm-member-post-dec",
                "return holder.value--;",
                6,
                Opcode::Sub,
                false,
            ),
            (
                "vm-member-pre-inc",
                "return ++holder.value;",
                6,
                Opcode::Add,
                true,
            ),
        ];

        for (label, source, reorder_depth, arithmetic, prefix) in cases {
            let compiled = compile_source(label, source);
            let words = compiled.codes[0]
                .bytecode
                .bytes
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>();
            let reorder =
                instruction_word(Opcode::Pop, VmType::Error, VmType::Variable) | reorder_depth;
            let arithmetic = instruction_word(arithmetic, VmType::Int, VmType::Variable);
            let reorder_position = words.iter().position(|word| *word == reorder).unwrap();
            let arithmetic_position = words.iter().position(|word| *word == arithmetic).unwrap();

            assert_eq!(
                reorder_position > arithmetic_position,
                prefix,
                "unexpected increment result ordering for {source}"
            );
        }
    }

    #[test]
    fn evaluates_increment_addresses_once() {
        let compiled = compile_source(
            "vm-increment-address-once",
            "a[irandom(3)]++; instance_find(0, 0).value++;",
        );
        let functions = &compiled.codes[0].bytecode.function_references;
        assert_eq!(
            functions
                .iter()
                .filter(|reference| reference.name == "irandom")
                .count(),
            1
        );
        assert_eq!(
            functions
                .iter()
                .filter(|reference| reference.name == "instance_find")
                .count(),
            1
        );
    }

    fn instruction_word(opcode: Opcode, first: VmType, second: VmType) -> u32 {
        ((opcode as u32) << 24) | ((first as u32 | ((second as u32) << 4)) << 16)
    }

    fn compile_source(label: &str, source: &str) -> CompiledProject {
        compile_source_with_constants(label, source, &[])
    }

    fn compile_source_with_constants(
        label: &str,
        source: &str,
        constants: &[(&str, &str)],
    ) -> CompiledProject {
        let root = temp_dir(label);
        fs::create_dir_all(root.join("Configs")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        let constants = constants
            .iter()
            .map(|(name, value)| format!("<constant name=\"{name}\">{value}</constant>"))
            .collect::<String>();
        fs::write(
            root.join("Tiny.project.gmx"),
            format!(
                "<assets><scripts><script>scripts\\test.gml</script></scripts><constants>{constants}</constants><Configs><Config>Configs\\Default</Config></Configs></assets>"
            ),
        )
        .unwrap();
        fs::write(
            root.join("Configs/Default.config.gmx"),
            "<Config><Options/></Config>",
        )
        .unwrap();
        fs::write(root.join("scripts/test.gml"), source).unwrap();
        let project = ProjectManifest::load(root.join("Tiny.project.gmx")).unwrap();
        let config = Config::load_from_project(&project, "Default").unwrap();
        let assets = Assets::load(&project, &config).unwrap();
        let analysis = analyze_assets(&assets).unwrap();
        let compiled = compile_vm(&assets, &analysis).unwrap();
        fs::remove_dir_all(root).unwrap();
        compiled
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("gmx-rs-{label}-{}-{nonce}", std::process::id()))
    }
}
