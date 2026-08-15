mod ast;
mod builtins;
mod chunks;
mod classifications;
mod codegen;
mod dnd;
mod lexer;
mod parser;
mod project;
mod semantic;
mod vm;

use std::error::Error;
use std::fmt;

pub use ast::*;
pub use chunks::add_vm_chunks;
pub use codegen::{
    CodegenDiagnostic, CompiledCode, CompiledProject, LocalVariable, VmSummary, compile_vm,
};
pub use dnd::{DndContext, DndError, GeneratedScript, LoweredActions, lower_actions};
pub use lexer::{Token, TokenKind, lex};
pub use parser::parse;
pub use project::{
    CheckSummary, CodeDiagnostic, CodeKind, CodeUnit, DndDiagnostic, action_code, check_assets,
    collect_code,
};
pub use semantic::{
    AnalysisDiagnostic, AnalyzedUnit, CallableSymbol, EnumInfo, NameAccess, NameBinding,
    NameResolution, ProjectAnalysis, ResourceType, SemanticDiagnostic, SemanticSummary, SymbolId,
    SymbolInfo, Symbols, ValueSymbol, analyze_assets,
};
pub use vm::{
    Condition, FunctionReference, Label, Opcode, StringReference, VariableKind, VariableReference,
    VmBuffer, VmBytecode, VmError, VmType,
};

pub fn function_classifications(project: &CompiledProject) -> i64 {
    classifications::WINDOWS_VM_BASELINE as i64
        | project.function_classifications
        | project
            .codes
            .iter()
            .flat_map(|code| &code.bytecode.function_references)
            .fold(0_u64, |flags, reference| {
                flags | classifications::get(&reference.name)
            }) as i64
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub(crate) fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}: {}",
            self.span.line, self.span.column, self.message
        )
    }
}

impl Error for Diagnostic {}
