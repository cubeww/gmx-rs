use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::assets::Assets;
use crate::settings::ConstantSource;

use super::ast::{AssignOp, Expr, ExprKind, PostfixOp, Program, Span, Stmt, StmtKind, UnaryOp};
use super::builtins;
use super::parse;
use super::project::{
    CheckSummary, CodeDiagnostic, CodeKind, CodeUnit, DndDiagnostic, collect_code,
    expand_code_macros,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct SymbolId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceType {
    Object,
    Sprite,
    Sound,
    Background,
    Path,
    Font,
    Timeline,
    Shader,
    Room,
    AudioGroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSymbol {
    BuiltinConstant,
    ConfiguredConstant {
        index: usize,
        source: ConstantSource,
    },
    Resource {
        kind: ResourceType,
        index: usize,
    },
    RoomInstance {
        id: i32,
    },
    Script {
        index: usize,
    },
    Enum {
        index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallableSymbol {
    BuiltinFunction,
    ExtensionFunction { id: i32 },
    Script { index: usize },
}

#[derive(Debug, Clone, Default)]
pub struct SymbolInfo {
    pub value: Option<ValueSymbol>,
    pub callable: Option<CallableSymbol>,
    pub builtin_variable: bool,
    value_priority: u8,
}

#[derive(Debug, Clone)]
pub struct EnumInfo {
    pub index: usize,
    pub members: HashMap<SymbolId, usize>,
}

#[derive(Debug)]
pub struct Symbols {
    names: Vec<Arc<str>>,
    ids: HashMap<Arc<str>, SymbolId>,
    info: Vec<SymbolInfo>,
    enums: HashMap<SymbolId, EnumInfo>,
}

impl Symbols {
    pub fn from_assets(assets: &Assets) -> Self {
        let mut symbols = Self::new();
        for name in builtins::FUNCTIONS.lines() {
            let id = symbols.intern(name);
            symbols.info[id.0 as usize].callable = Some(CallableSymbol::BuiltinFunction);
        }
        for name in builtins::CONSTANTS.lines() {
            let id = symbols.intern(name);
            symbols.set_value(id, ValueSymbol::BuiltinConstant, 10);
        }
        for name in builtins::VARIABLES.lines() {
            let id = symbols.intern(name);
            symbols.info[id.0 as usize].builtin_variable = true;
        }

        for (index, constant) in assets.settings.constants.iter().enumerate() {
            let id = symbols.intern(&constant.name);
            symbols.set_value(
                id,
                ValueSymbol::ConfiguredConstant {
                    index,
                    source: constant.source,
                },
                30,
            );
        }

        let config = &assets.settings.options.config;
        let target_mask = assets.settings.target_mask;
        for extension in &assets.extensions {
            if !extension.enabled_for(config, target_mask) {
                continue;
            }
            for file in &extension.files {
                if !file.enabled_for(config, target_mask) {
                    continue;
                }
                for function in &file.functions {
                    let id = symbols.intern(&function.name);
                    if !matches!(
                        symbols.info[id.0 as usize].callable,
                        Some(CallableSymbol::Script { .. })
                    ) {
                        symbols.info[id.0 as usize].callable =
                            Some(CallableSymbol::ExtensionFunction { id: function.id });
                    }
                }
            }
        }

        for script in &assets.scripts {
            let id = symbols.intern(&script.name);
            symbols.info[id.0 as usize].callable = Some(CallableSymbol::Script {
                index: script.index,
            });
            symbols.set_value(
                id,
                ValueSymbol::Script {
                    index: script.index,
                },
                5,
            );
        }

        for object in &assets.objects {
            symbols.add_resource(&object.name, ResourceType::Object, object.index);
        }
        for sprite in &assets.sprites {
            symbols.add_resource(&sprite.name, ResourceType::Sprite, sprite.index);
        }
        for sound in &assets.sounds {
            symbols.add_resource(&sound.name, ResourceType::Sound, sound.index);
        }
        for background in &assets.backgrounds {
            symbols.add_resource(&background.name, ResourceType::Background, background.index);
        }
        for path in &assets.paths {
            symbols.add_resource(&path.name, ResourceType::Path, path.index);
        }
        for font in &assets.fonts {
            symbols.add_resource(&font.name, ResourceType::Font, font.index);
        }
        for timeline in &assets.timelines {
            symbols.add_resource(&timeline.name, ResourceType::Timeline, timeline.index);
        }
        for shader in &assets.shaders {
            symbols.add_resource(&shader.name, ResourceType::Shader, shader.index);
        }
        for room in &assets.rooms {
            symbols.add_resource(&room.name, ResourceType::Room, room.index);
            for instance in &room.instances {
                let id = symbols.intern(&instance.name);
                symbols.set_value(id, ValueSymbol::RoomInstance { id: instance.id }, 40);
            }
        }
        for group in &assets.settings.audio_groups {
            symbols.add_resource(&group.name, ResourceType::AudioGroup, group.index);
        }
        symbols
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn id(&self, name: &str) -> Option<SymbolId> {
        self.ids.get(name).copied()
    }

    pub fn name(&self, id: SymbolId) -> &str {
        &self.names[id.0 as usize]
    }

    pub fn info(&self, id: SymbolId) -> &SymbolInfo {
        &self.info[id.0 as usize]
    }

    pub fn enum_info(&self, id: SymbolId) -> Option<&EnumInfo> {
        self.enums.get(&id)
    }

    fn new() -> Self {
        Self {
            names: Vec::new(),
            ids: HashMap::new(),
            info: Vec::new(),
            enums: HashMap::new(),
        }
    }

    fn intern(&mut self, name: &str) -> SymbolId {
        if let Some(id) = self.id(name) {
            return id;
        }
        let id = SymbolId(self.names.len() as u32);
        let name: Arc<str> = Arc::from(name);
        self.ids.insert(name.clone(), id);
        self.names.push(name);
        self.info.push(SymbolInfo::default());
        id
    }

    fn set_value(&mut self, id: SymbolId, value: ValueSymbol, priority: u8) {
        let info = &mut self.info[id.0 as usize];
        if priority >= info.value_priority {
            info.value = Some(value);
            info.value_priority = priority;
        }
    }

    fn add_resource(&mut self, name: &str, kind: ResourceType, index: usize) {
        let id = self.intern(name);
        self.set_value(id, ValueSymbol::Resource { kind, index }, 40);
    }

    fn define_enum(&mut self, name: SymbolId, members: Vec<SymbolId>) {
        let index = self.enums.len();
        let members = members
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect();
        self.enums.insert(name, EnumInfo { index, members });
        self.set_value(name, ValueSymbol::Enum { index }, 35);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameAccess {
    Read,
    Write,
    ReadWrite,
    Call,
    Declare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameBinding {
    LocalVariable {
        slot: u32,
    },
    GlobalVariable,
    InstanceVariable,
    BuiltinVariable,
    BuiltinConstant,
    ConfiguredConstant {
        index: usize,
    },
    Resource {
        kind: ResourceType,
        index: usize,
    },
    RoomInstance {
        id: i32,
    },
    Script {
        index: usize,
    },
    BuiltinFunction,
    ExtensionFunction {
        id: i32,
    },
    Enum {
        index: usize,
    },
    EnumMember {
        enum_symbol: SymbolId,
        member_index: usize,
    },
}

impl NameBinding {
    fn writable(self) -> bool {
        matches!(
            self,
            Self::LocalVariable { .. }
                | Self::GlobalVariable
                | Self::InstanceVariable
                | Self::BuiltinVariable
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameResolution {
    pub symbol: SymbolId,
    pub binding: NameBinding,
    pub access: NameAccess,
    pub span: Span,
}

#[derive(Debug)]
pub struct AnalyzedUnit<'a> {
    pub kind: CodeKind,
    pub name: String,
    pub vm_name: String,
    pub source: &'a Path,
    pub code: std::borrow::Cow<'a, str>,
    pub program: Program,
    pub names: Vec<NameResolution>,
    pub locals: Vec<SymbolId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticSummary {
    pub syntax: CheckSummary,
    pub symbols: usize,
    pub names: usize,
    pub locals: usize,
    pub global_variables: usize,
}

#[derive(Debug)]
pub struct ProjectAnalysis<'a> {
    pub symbols: Symbols,
    pub units: Vec<AnalyzedUnit<'a>>,
    pub summary: SemanticSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticDiagnostic {
    pub kind: CodeKind,
    pub name: String,
    pub source: PathBuf,
    pub span: Span,
    pub message: String,
}

impl fmt::Display for SemanticDiagnostic {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisDiagnostic {
    Syntax(CodeDiagnostic),
    Dnd(DndDiagnostic),
    Semantic(SemanticDiagnostic),
}

impl fmt::Display for AnalysisDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => error.fmt(formatter),
            Self::Dnd(error) => error.fmt(formatter),
            Self::Semantic(error) => error.fmt(formatter),
        }
    }
}

pub fn analyze_assets(assets: &Assets) -> Result<ProjectAnalysis<'_>, Vec<AnalysisDiagnostic>> {
    let mut units = collect_code(assets).map_err(|errors| {
        errors
            .into_iter()
            .map(AnalysisDiagnostic::Dnd)
            .collect::<Vec<_>>()
    })?;
    let macro_errors = expand_code_macros(assets, &mut units);
    if !macro_errors.is_empty() {
        return Err(macro_errors
            .into_iter()
            .map(AnalysisDiagnostic::Syntax)
            .collect());
    }
    let parsed: Vec<_> = units.par_iter().map(|unit| parse(&unit.code)).collect();
    let mut programs = Vec::with_capacity(units.len());
    let mut syntax_errors = Vec::new();
    for (unit, result) in units.iter().zip(parsed) {
        match result {
            Ok(program) => programs.push(program),
            Err(errors) => {
                syntax_errors.extend(errors.into_iter().map(|diagnostic| {
                    AnalysisDiagnostic::Syntax(CodeDiagnostic {
                        kind: unit.kind,
                        name: unit.name.clone(),
                        source: unit.source.to_path_buf(),
                        diagnostic,
                    })
                }));
            }
        }
    }
    if !syntax_errors.is_empty() {
        return Err(syntax_errors);
    }

    let mut symbols = Symbols::from_assets(assets);
    for unit in &units {
        if let CodeKind::GeneratedScript { index } = unit.kind {
            let id = symbols.intern(&unit.name);
            symbols.info[id.0 as usize].callable = Some(CallableSymbol::Script { index });
            symbols.set_value(id, ValueSymbol::Script { index }, 5);
        }
    }
    for (unit, program) in units.iter().zip(&programs) {
        collect_names(&mut symbols, &unit.code, &program.statements);
    }
    let mut global_variables = HashSet::new();
    for (unit, program) in units.iter().zip(&programs) {
        collect_project_declarations(
            &mut symbols,
            &mut global_variables,
            &unit.code,
            &program.statements,
        );
    }

    let resolved: Vec<_> = units
        .par_iter()
        .zip(programs.par_iter())
        .map(|(unit, program)| Resolver::new(&symbols, &global_variables, unit, program).run())
        .collect();
    let mut semantic_errors = Vec::new();
    for result in &resolved {
        semantic_errors.extend(
            result
                .errors
                .iter()
                .cloned()
                .map(AnalysisDiagnostic::Semantic),
        );
    }
    if !semantic_errors.is_empty() {
        return Err(semantic_errors);
    }

    let syntax = CheckSummary {
        units: units.len(),
        tokens: programs.iter().map(|program| program.token_count).sum(),
        statements: programs
            .iter()
            .flat_map(|program| &program.statements)
            .map(statement_count)
            .sum(),
    };
    let names = resolved.iter().map(|result| result.names.len()).sum();
    let locals = resolved.iter().map(|result| result.locals.len()).sum();
    let summary = SemanticSummary {
        syntax,
        symbols: symbols.len(),
        names,
        locals,
        global_variables: global_variables.len(),
    };
    let units = units
        .into_iter()
        .zip(programs)
        .zip(resolved)
        .map(|((unit, program), resolved)| AnalyzedUnit {
            kind: unit.kind,
            name: unit.name,
            vm_name: unit.vm_name,
            source: unit.source,
            code: unit.code,
            program,
            names: resolved.names,
            locals: resolved.locals,
        })
        .collect();
    Ok(ProjectAnalysis {
        symbols,
        units,
        summary,
    })
}

struct ResolveResult {
    names: Vec<NameResolution>,
    locals: Vec<SymbolId>,
    errors: Vec<SemanticDiagnostic>,
}

struct Resolver<'a> {
    symbols: &'a Symbols,
    global_variables: &'a HashSet<SymbolId>,
    unit: &'a CodeUnit<'a>,
    program: &'a Program,
    locals: HashMap<SymbolId, u32>,
    local_order: Vec<SymbolId>,
    names: Vec<NameResolution>,
    errors: Vec<SemanticDiagnostic>,
}

impl<'a> Resolver<'a> {
    fn new(
        symbols: &'a Symbols,
        global_variables: &'a HashSet<SymbolId>,
        unit: &'a CodeUnit<'a>,
        program: &'a Program,
    ) -> Self {
        Self {
            symbols,
            global_variables,
            unit,
            program,
            locals: HashMap::new(),
            local_order: Vec::new(),
            names: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn run(mut self) -> ResolveResult {
        for statement in &self.program.statements {
            self.collect_locals(statement);
        }
        for statement in &self.program.statements {
            self.statement(statement);
        }
        ResolveResult {
            names: self.names,
            locals: self.local_order,
            errors: self.errors,
        }
    }

    fn collect_locals(&mut self, statement: &Stmt) {
        match &statement.kind {
            StmtKind::Var {
                global: false,
                declarations,
            } => {
                for declaration in declarations {
                    let id = self.symbol(declaration.name);
                    if !self.locals.contains_key(&id) {
                        let slot = self.local_order.len() as u32;
                        self.locals.insert(id, slot);
                        self.local_order.push(id);
                    }
                }
            }
            StmtKind::Block(statements) => {
                for statement in statements {
                    self.collect_locals(statement);
                }
            }
            StmtKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.collect_locals(then_branch);
                if let Some(branch) = else_branch {
                    self.collect_locals(branch);
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::DoUntil { body, .. }
            | StmtKind::Repeat { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::Switch { body, .. } => self.collect_locals(body),
            StmtKind::For {
                initializer, body, ..
            } => {
                if let Some(initializer) = initializer {
                    self.collect_locals(initializer);
                }
                self.collect_locals(body);
            }
            _ => {}
        }
    }

    fn statement(&mut self, statement: &Stmt) {
        match &statement.kind {
            StmtKind::Empty
            | StmtKind::Default
            | StmtKind::Exit
            | StmtKind::Break
            | StmtKind::Continue => {}
            StmtKind::Block(statements) => {
                for statement in statements {
                    self.statement(statement);
                }
            }
            StmtKind::Var {
                global,
                declarations,
            } => {
                for declaration in declarations {
                    let id = self.symbol(declaration.name);
                    let binding = if *global {
                        NameBinding::GlobalVariable
                    } else {
                        NameBinding::LocalVariable {
                            slot: self.locals[&id],
                        }
                    };
                    self.check_declaration(id, declaration.name);
                    self.record(id, binding, NameAccess::Declare, declaration.name);
                    if let Some(value) = &declaration.value {
                        self.expression(value, NameAccess::Read);
                    }
                }
            }
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.expression(condition, NameAccess::Read);
                self.statement(then_branch);
                if let Some(branch) = else_branch {
                    self.statement(branch);
                }
            }
            StmtKind::While { condition, body } => {
                self.expression(condition, NameAccess::Read);
                self.statement(body);
            }
            StmtKind::DoUntil { body, condition } => {
                self.statement(body);
                self.expression(condition, NameAccess::Read);
            }
            StmtKind::For {
                initializer,
                condition,
                step,
                body,
            } => {
                if let Some(initializer) = initializer {
                    self.statement(initializer);
                }
                if let Some(condition) = condition {
                    self.expression(condition, NameAccess::Read);
                }
                if let Some(step) = step {
                    self.expression(step, NameAccess::Read);
                }
                self.statement(body);
            }
            StmtKind::Repeat { count, body } => {
                self.expression(count, NameAccess::Read);
                self.statement(body);
            }
            StmtKind::With { target, body } => {
                self.expression(target, NameAccess::Read);
                self.statement(body);
            }
            StmtKind::Switch { value, body } => {
                self.expression(value, NameAccess::Read);
                self.statement(body);
            }
            StmtKind::Case(value) => self.expression(value, NameAccess::Read),
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.expression(value, NameAccess::Read);
                }
            }
            StmtKind::Enum { name, members } => {
                let enum_symbol = self.symbol(*name);
                let index = self
                    .symbols
                    .enum_info(enum_symbol)
                    .map_or(0, |info| info.index);
                self.record(
                    enum_symbol,
                    NameBinding::Enum { index },
                    NameAccess::Declare,
                    *name,
                );
                for member in members {
                    let member_symbol = self.symbol(member.name);
                    let member_index = self
                        .symbols
                        .enum_info(enum_symbol)
                        .and_then(|info| info.members.get(&member_symbol))
                        .copied()
                        .unwrap_or(0);
                    self.record(
                        member_symbol,
                        NameBinding::EnumMember {
                            enum_symbol,
                            member_index,
                        },
                        NameAccess::Declare,
                        member.name,
                    );
                    if let Some(value) = &member.value {
                        self.expression(value, NameAccess::Read);
                    }
                }
            }
            StmtKind::Expr(expression) => self.expression(expression, NameAccess::Read),
        }
    }

    fn expression(&mut self, expression: &Expr, access: NameAccess) {
        match &expression.kind {
            ExprKind::Identifier => self.identifier(expression.span, access),
            ExprKind::Number | ExprKind::String => {}
            ExprKind::Group(value) => self.expression(value, access),
            ExprKind::Array(values) => {
                for value in values {
                    self.expression(value, NameAccess::Read);
                }
            }
            ExprKind::Unary { op, value } => {
                let access = if matches!(op, UnaryOp::PreIncrement | UnaryOp::PreDecrement) {
                    NameAccess::ReadWrite
                } else {
                    NameAccess::Read
                };
                self.expression(value, access);
            }
            ExprKind::Binary { left, right, .. } => {
                self.expression(left, NameAccess::Read);
                self.expression(right, NameAccess::Read);
            }
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                self.expression(condition, NameAccess::Read);
                self.expression(then_value, NameAccess::Read);
                self.expression(else_value, NameAccess::Read);
            }
            ExprKind::Assign { op, target, value } => {
                let target_access = if *op == AssignOp::Set {
                    NameAccess::Write
                } else {
                    NameAccess::ReadWrite
                };
                self.expression(target, target_access);
                self.expression(value, NameAccess::Read);
            }
            ExprKind::Call { callee, arguments } => {
                self.call(callee);
                for argument in arguments {
                    self.expression(argument, NameAccess::Read);
                }
            }
            ExprKind::Index {
                target, indices, ..
            } => {
                self.expression(target, access);
                for index in indices {
                    self.expression(index, NameAccess::Read);
                }
            }
            ExprKind::Member { target, name } => self.member(target, *name, access),
            ExprKind::Postfix { op, target } => {
                debug_assert!(matches!(op, PostfixOp::Increment | PostfixOp::Decrement));
                self.expression(target, NameAccess::ReadWrite);
            }
        }
    }

    fn identifier(&mut self, span: Span, access: NameAccess) {
        let id = self.symbol(span);
        let binding = self.value_or_variable(id);
        if matches!(access, NameAccess::Write | NameAccess::ReadWrite) && !binding.writable() {
            self.error(span, format!("cannot assign to {}", self.symbols.name(id)));
        }
        self.record(id, binding, access, span);
    }

    fn call(&mut self, callee: &Expr) {
        if matches!(callee.kind, ExprKind::Identifier) {
            let id = self.symbol(callee.span);
            let binding = match self.symbols.info(id).callable {
                Some(CallableSymbol::BuiltinFunction) => NameBinding::BuiltinFunction,
                Some(CallableSymbol::ExtensionFunction { id }) => {
                    NameBinding::ExtensionFunction { id }
                }
                Some(CallableSymbol::Script { index }) => NameBinding::Script { index },
                None => {
                    self.error(
                        callee.span,
                        format!("unknown function {}", self.symbols.name(id)),
                    );
                    self.value_or_variable(id)
                }
            };
            self.record(id, binding, NameAccess::Call, callee.span);
        } else {
            self.expression(callee, NameAccess::Read);
        }
    }

    fn member(&mut self, target: &Expr, name: Span, access: NameAccess) {
        self.expression(target, NameAccess::Read);
        let id = self.symbol(name);
        let binding = if let ExprKind::Identifier = target.kind {
            let target_id = self.symbol(target.span);
            match self.symbols.name(target_id) {
                "global" => NameBinding::GlobalVariable,
                "local" => self
                    .locals
                    .get(&id)
                    .copied()
                    .map_or(NameBinding::InstanceVariable, |slot| {
                        NameBinding::LocalVariable { slot }
                    }),
                _ => self
                    .symbols
                    .enum_info(target_id)
                    .and_then(|info| {
                        info.members
                            .get(&id)
                            .map(|member_index| NameBinding::EnumMember {
                                enum_symbol: target_id,
                                member_index: *member_index,
                            })
                    })
                    .unwrap_or(NameBinding::InstanceVariable),
            }
        } else {
            NameBinding::InstanceVariable
        };
        if matches!(access, NameAccess::Write | NameAccess::ReadWrite) && !binding.writable() {
            self.error(name, format!("cannot assign to {}", self.symbols.name(id)));
        }
        self.record(id, binding, access, name);
    }

    fn value_or_variable(&self, id: SymbolId) -> NameBinding {
        let info = self.symbols.info(id);
        if let Some(value) = info.value {
            return match value {
                ValueSymbol::BuiltinConstant => NameBinding::BuiltinConstant,
                ValueSymbol::ConfiguredConstant { index, .. } => {
                    NameBinding::ConfiguredConstant { index }
                }
                ValueSymbol::Resource { kind, index } => NameBinding::Resource { kind, index },
                ValueSymbol::RoomInstance { id } => NameBinding::RoomInstance { id },
                ValueSymbol::Script { index } => NameBinding::Script { index },
                ValueSymbol::Enum { index } => NameBinding::Enum { index },
            };
        }
        if let Some(slot) = self.locals.get(&id) {
            return NameBinding::LocalVariable { slot: *slot };
        }
        if self.global_variables.contains(&id) {
            return NameBinding::GlobalVariable;
        }
        if info.builtin_variable {
            return NameBinding::BuiltinVariable;
        }
        NameBinding::InstanceVariable
    }

    fn check_declaration(&mut self, id: SymbolId, span: Span) {
        let info = self.symbols.info(id);
        if info.value.is_some() || info.callable.is_some() {
            self.error(
                span,
                format!(
                    "{} is a constant, resource, script, or function name",
                    self.symbols.name(id)
                ),
            );
        }
    }

    fn symbol(&self, span: Span) -> SymbolId {
        self.symbols
            .id(text(&self.unit.code, span))
            .expect("all AST names must be interned before resolution")
    }

    fn record(&mut self, symbol: SymbolId, binding: NameBinding, access: NameAccess, span: Span) {
        self.names.push(NameResolution {
            symbol,
            binding,
            access,
            span,
        });
    }

    fn error(&mut self, span: Span, message: String) {
        self.errors.push(SemanticDiagnostic {
            kind: self.unit.kind,
            name: self.unit.name.clone(),
            source: self.unit.source.to_path_buf(),
            span,
            message,
        });
    }
}

fn collect_names(symbols: &mut Symbols, source: &str, statements: &[Stmt]) {
    for statement in statements {
        visit_statement_names(statement, &mut |span| {
            symbols.intern(text(source, span));
        });
    }
}

fn collect_project_declarations(
    symbols: &mut Symbols,
    globals: &mut HashSet<SymbolId>,
    source: &str,
    statements: &[Stmt],
) {
    for statement in statements {
        match &statement.kind {
            StmtKind::Var {
                global: true,
                declarations,
            } => {
                globals.extend(
                    declarations
                        .iter()
                        .map(|declaration| symbols.id(text(source, declaration.name)).unwrap()),
                );
            }
            StmtKind::Enum { name, members } => {
                let name = symbols.id(text(source, *name)).unwrap();
                let members = members
                    .iter()
                    .map(|member| symbols.id(text(source, member.name)).unwrap())
                    .collect();
                symbols.define_enum(name, members);
            }
            StmtKind::Block(nested) => {
                collect_project_declarations(symbols, globals, source, nested);
            }
            StmtKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                collect_project_declarations(
                    symbols,
                    globals,
                    source,
                    std::slice::from_ref(then_branch),
                );
                if let Some(branch) = else_branch {
                    collect_project_declarations(
                        symbols,
                        globals,
                        source,
                        std::slice::from_ref(branch),
                    );
                }
            }
            StmtKind::While { body, .. }
            | StmtKind::DoUntil { body, .. }
            | StmtKind::Repeat { body, .. }
            | StmtKind::With { body, .. }
            | StmtKind::Switch { body, .. } => {
                collect_project_declarations(symbols, globals, source, std::slice::from_ref(body))
            }
            StmtKind::For {
                initializer, body, ..
            } => {
                if let Some(initializer) = initializer {
                    collect_project_declarations(
                        symbols,
                        globals,
                        source,
                        std::slice::from_ref(initializer),
                    );
                }
                collect_project_declarations(symbols, globals, source, std::slice::from_ref(body));
            }
            _ => {}
        }
    }
}

fn visit_statement_names(statement: &Stmt, visitor: &mut impl FnMut(Span)) {
    match &statement.kind {
        StmtKind::Empty
        | StmtKind::Default
        | StmtKind::Exit
        | StmtKind::Break
        | StmtKind::Continue => {}
        StmtKind::Block(statements) => {
            for statement in statements {
                visit_statement_names(statement, visitor);
            }
        }
        StmtKind::Var { declarations, .. } => {
            for declaration in declarations {
                visitor(declaration.name);
                if let Some(value) = &declaration.value {
                    visit_expression_names(value, visitor);
                }
            }
        }
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            visit_expression_names(condition, visitor);
            visit_statement_names(then_branch, visitor);
            if let Some(branch) = else_branch {
                visit_statement_names(branch, visitor);
            }
        }
        StmtKind::While { condition, body } => {
            visit_expression_names(condition, visitor);
            visit_statement_names(body, visitor);
        }
        StmtKind::DoUntil { body, condition } => {
            visit_statement_names(body, visitor);
            visit_expression_names(condition, visitor);
        }
        StmtKind::For {
            initializer,
            condition,
            step,
            body,
        } => {
            if let Some(initializer) = initializer {
                visit_statement_names(initializer, visitor);
            }
            if let Some(condition) = condition {
                visit_expression_names(condition, visitor);
            }
            if let Some(step) = step {
                visit_expression_names(step, visitor);
            }
            visit_statement_names(body, visitor);
        }
        StmtKind::Repeat { count, body } => {
            visit_expression_names(count, visitor);
            visit_statement_names(body, visitor);
        }
        StmtKind::With { target, body } => {
            visit_expression_names(target, visitor);
            visit_statement_names(body, visitor);
        }
        StmtKind::Switch { value, body } => {
            visit_expression_names(value, visitor);
            visit_statement_names(body, visitor);
        }
        StmtKind::Case(value) => visit_expression_names(value, visitor),
        StmtKind::Return(value) => {
            if let Some(value) = value {
                visit_expression_names(value, visitor);
            }
        }
        StmtKind::Enum { name, members } => {
            visitor(*name);
            for member in members {
                visitor(member.name);
                if let Some(value) = &member.value {
                    visit_expression_names(value, visitor);
                }
            }
        }
        StmtKind::Expr(expression) => visit_expression_names(expression, visitor),
    }
}

fn visit_expression_names(expression: &Expr, visitor: &mut impl FnMut(Span)) {
    match &expression.kind {
        ExprKind::Identifier => visitor(expression.span),
        ExprKind::Number | ExprKind::String => {}
        ExprKind::Group(value)
        | ExprKind::Unary { value, .. }
        | ExprKind::Postfix { target: value, .. } => visit_expression_names(value, visitor),
        ExprKind::Array(values) => {
            for value in values {
                visit_expression_names(value, visitor);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            visit_expression_names(left, visitor);
            visit_expression_names(right, visitor);
        }
        ExprKind::Conditional {
            condition,
            then_value,
            else_value,
        } => {
            visit_expression_names(condition, visitor);
            visit_expression_names(then_value, visitor);
            visit_expression_names(else_value, visitor);
        }
        ExprKind::Assign { target, value, .. } => {
            visit_expression_names(target, visitor);
            visit_expression_names(value, visitor);
        }
        ExprKind::Call { callee, arguments } => {
            visit_expression_names(callee, visitor);
            for argument in arguments {
                visit_expression_names(argument, visitor);
            }
        }
        ExprKind::Index {
            target, indices, ..
        } => {
            visit_expression_names(target, visitor);
            for index in indices {
                visit_expression_names(index, visitor);
            }
        }
        ExprKind::Member { target, name } => {
            visit_expression_names(target, visitor);
            visitor(*name);
        }
    }
}

fn statement_count(statement: &Stmt) -> usize {
    1 + match &statement.kind {
        StmtKind::Block(statements) => statements.iter().map(statement_count).sum(),
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => statement_count(then_branch) + else_branch.as_deref().map_or(0, statement_count),
        StmtKind::While { body, .. }
        | StmtKind::DoUntil { body, .. }
        | StmtKind::Repeat { body, .. }
        | StmtKind::With { body, .. }
        | StmtKind::Switch { body, .. } => statement_count(body),
        StmtKind::For {
            initializer, body, ..
        } => initializer.as_deref().map_or(0, statement_count) + statement_count(body),
        _ => 0,
    }
}

fn text(source: &str, span: Span) -> &str {
    &source[span.start as usize..span.end as usize]
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    fn resolve(source: &str, configure: impl FnOnce(&mut Symbols)) -> ResolveResult {
        let mut symbols = Symbols::new();
        configure(&mut symbols);
        let program = parse(source).unwrap();
        collect_names(&mut symbols, source, &program.statements);
        let mut globals = HashSet::new();
        collect_project_declarations(&mut symbols, &mut globals, source, &program.statements);
        let unit = CodeUnit {
            kind: CodeKind::Script,
            name: "test".to_owned(),
            vm_name: "gml_Script_test".to_owned(),
            source: Path::new("test.gml"),
            code: Cow::Borrowed(source),
        };
        Resolver::new(&symbols, &globals, &unit, &program).run()
    }

    fn define_value(symbols: &mut Symbols, name: &str, value: ValueSymbol) {
        let id = symbols.intern(name);
        symbols.set_value(id, value, 40);
    }

    fn define_callable(symbols: &mut Symbols, name: &str, callable: CallableSymbol) {
        let id = symbols.intern(name);
        symbols.info[id.0 as usize].callable = Some(callable);
    }

    #[test]
    fn runner_tables_have_expected_entries() {
        assert_eq!(builtins::FUNCTIONS.lines().count(), 2733);
        assert_eq!(builtins::CONSTANTS.lines().count(), 507);
        assert_eq!(builtins::VARIABLES.lines().count(), 212);
        assert!(
            builtins::FUNCTIONS
                .lines()
                .any(|name| name == "instance_create")
        );
        assert!(
            builtins::FUNCTIONS
                .lines()
                .any(|name| name == "audio_play_sound")
        );
        assert!(builtins::CONSTANTS.lines().any(|name| name == "c_red"));
        assert!(builtins::VARIABLES.lines().any(|name| name == "room_speed"));
    }

    #[test]
    fn resolves_constants_variables_scripts_and_functions() {
        let result = resolve(
            "var value = c_red; global.score = value; room_speed += 1; custom = value; scr_test(); show_debug_message(value);",
            |symbols| {
                define_value(symbols, "c_red", ValueSymbol::BuiltinConstant);
                define_value(symbols, "global", ValueSymbol::BuiltinConstant);
                let room_speed = symbols.intern("room_speed");
                symbols.info[room_speed.0 as usize].builtin_variable = true;
                define_callable(symbols, "scr_test", CallableSymbol::Script { index: 3 });
                define_callable(
                    symbols,
                    "show_debug_message",
                    CallableSymbol::BuiltinFunction,
                );
            },
        );
        assert!(result.errors.is_empty());
        assert_eq!(result.locals.len(), 1);
        assert!(
            result
                .names
                .iter()
                .any(|name| matches!(name.binding, NameBinding::LocalVariable { slot: 0 }))
        );
        assert!(
            result
                .names
                .iter()
                .any(|name| name.binding == NameBinding::GlobalVariable)
        );
        assert!(
            result
                .names
                .iter()
                .any(|name| name.binding == NameBinding::BuiltinVariable)
        );
        assert!(
            result
                .names
                .iter()
                .any(|name| name.binding == NameBinding::InstanceVariable)
        );
        assert!(
            result
                .names
                .iter()
                .any(|name| name.binding == NameBinding::Script { index: 3 })
        );
        assert!(
            result
                .names
                .iter()
                .any(|name| name.binding == NameBinding::BuiltinFunction)
        );
    }

    #[test]
    fn resolves_globalvar_and_enum_members_project_wide() {
        let result = resolve(
            "globalvar shared; enum State { Idle, Run = 4 } shared = State.Run;",
            |_| {},
        );
        assert!(result.errors.is_empty());
        assert!(result.names.iter().any(|name| {
            name.binding == NameBinding::GlobalVariable && name.access == NameAccess::Write
        }));
        assert!(result.names.iter().any(|name| matches!(
            name.binding,
            NameBinding::EnumMember {
                member_index: 1,
                ..
            }
        )));
    }

    #[test]
    fn rejects_constant_writes_and_unknown_calls() {
        let result = resolve("c_red = 1; missing_call();", |symbols| {
            define_value(symbols, "c_red", ValueSymbol::BuiltinConstant);
        });
        assert_eq!(result.errors.len(), 2);
        assert!(result.errors[0].message.contains("cannot assign"));
        assert!(result.errors[1].message.contains("unknown function"));
    }
}
