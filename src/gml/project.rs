use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::assets::{Action, Assets, script_parts};

use super::macros::MacroTable;
use super::{Diagnostic, Program, Stmt, StmtKind, TokenKind, lex, parse};
use super::{DndContext, DndError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    Script,
    GeneratedScript { index: usize },
    ObjectEvent,
    Timeline,
    RoomCreation,
    RoomInstance,
    Extension,
    Global,
}

impl fmt::Display for CodeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Script => "script",
            Self::GeneratedScript { .. } => "generated script",
            Self::ObjectEvent => "object event",
            Self::Timeline => "timeline",
            Self::RoomCreation => "room creation code",
            Self::RoomInstance => "room instance code",
            Self::Extension => "extension code",
            Self::Global => "global initialization code",
        })
    }
}

#[derive(Debug)]
pub struct CodeUnit<'a> {
    pub kind: CodeKind,
    pub name: String,
    pub vm_name: String,
    pub source: &'a Path,
    pub code: Cow<'a, str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckSummary {
    pub units: usize,
    pub tokens: usize,
    pub statements: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeDiagnostic {
    pub kind: CodeKind,
    pub name: String,
    pub source: PathBuf,
    pub diagnostic: Diagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DndDiagnostic {
    pub kind: CodeKind,
    pub name: String,
    pub source: PathBuf,
    pub error: DndError,
}

impl fmt::Display for DndDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} {}: {}",
            self.source.display(),
            self.kind,
            self.name,
            self.error
        )
    }
}

impl fmt::Display for CodeDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{}: {} {}: {}",
            self.source.display(),
            self.diagnostic.span.line,
            self.diagnostic.span.column,
            self.kind,
            self.name,
            self.diagnostic.message
        )
    }
}

pub fn collect_code(assets: &Assets) -> Result<Vec<CodeUnit<'_>>, Vec<DndDiagnostic>> {
    let mut units = Vec::new();
    let mut generated_units = Vec::new();
    let mut object_units = Vec::new();
    let mut timeline_units = Vec::new();
    let mut errors = Vec::new();
    let mut dnd = DndContext::new();
    let mut generated_script_index = assets.scripts.len();

    for script in &assets.scripts {
        push_code(
            &mut units,
            CodeKind::Script,
            script.name.clone(),
            format!("gml_Script_{}", script.name),
            &script.source,
            Cow::Borrowed(&script.code),
        );
    }

    for object in &assets.objects {
        for event in object.events.iter().flatten() {
            let name = format!("{}[{},{}]", object.name, event.event_type, event.subtype);
            match dnd.lower(&event.actions) {
                Ok(lowered) => {
                    push_code(
                        &mut object_units,
                        CodeKind::ObjectEvent,
                        name,
                        format!(
                            "gml_Object_{}_{}_{}",
                            object.name.replace(' ', "_"),
                            event_name(event.event_type),
                            event.subtype
                        ),
                        &object.source,
                        Cow::Owned(lowered.code),
                    );
                    for script in lowered.scripts {
                        push_code(
                            &mut generated_units,
                            CodeKind::GeneratedScript {
                                index: generated_script_index,
                            },
                            script.name.clone(),
                            format!("gml_Script_{}", script.name),
                            &object.source,
                            Cow::Owned(script.code),
                        );
                        generated_script_index += 1;
                    }
                }
                Err(error) => errors.push(DndDiagnostic {
                    kind: CodeKind::ObjectEvent,
                    name,
                    source: object.source.clone(),
                    error,
                }),
            }
        }
    }

    for timeline in &assets.timelines {
        let mut compiled_entry = 0_usize;
        for entry in &timeline.entries {
            let name = format!("{}[step={}]", timeline.name, entry.step);
            match dnd.lower(&entry.actions) {
                Ok(lowered) => {
                    if push_code(
                        &mut timeline_units,
                        CodeKind::Timeline,
                        name,
                        format!(
                            "Timeline_{}_{}",
                            sanitize_name(&timeline.name),
                            compiled_entry
                        ),
                        &timeline.source,
                        Cow::Owned(lowered.code),
                    ) {
                        compiled_entry += 1;
                    }
                    for script in lowered.scripts {
                        push_code(
                            &mut generated_units,
                            CodeKind::GeneratedScript {
                                index: generated_script_index,
                            },
                            script.name.clone(),
                            format!("gml_Script_{}", script.name),
                            &timeline.source,
                            Cow::Owned(script.code),
                        );
                        generated_script_index += 1;
                    }
                }
                Err(error) => errors.push(DndDiagnostic {
                    kind: CodeKind::Timeline,
                    name,
                    source: timeline.source.clone(),
                    error,
                }),
            }
        }
    }

    // The official loader appends scripts synthesized by question actions to
    // the script asset list before object and timeline code is compiled.
    units.append(&mut generated_units);
    units.append(&mut object_units);
    units.append(&mut timeline_units);

    for room in &assets.rooms {
        push_code(
            &mut units,
            CodeKind::RoomCreation,
            room.name.clone(),
            format!("gml_Room_{}_Create", sanitize_name(&room.name)),
            &room.source,
            Cow::Borrowed(&room.code),
        );
        for instance in &room.instances {
            let code_index = units.len();
            push_code(
                &mut units,
                CodeKind::RoomInstance,
                format!("{}.{}", room.name, instance.name),
                format!(
                    "gml_RoomCC_{}_{}_Create",
                    sanitize_name(&room.name),
                    code_index
                ),
                &room.source,
                Cow::Borrowed(&instance.code),
            );
        }
    }

    let config = &assets.settings.options.config;
    let target_mask = assets.settings.target_mask;
    for extension in &assets.extensions {
        if !extension.enabled_for(config, target_mask) {
            continue;
        }
        for file in &extension.files {
            if file.kind != 2 || !file.used || !file.enabled_for(config, target_mask) {
                continue;
            }
            let Some(bytes) = assets.binary(&file.source) else {
                continue;
            };
            let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
            let code = String::from_utf8_lossy(bytes);
            let default_name = Path::new(&file.filename)
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or(&file.filename);
            for part in script_parts(default_name, &code) {
                push_code(
                    &mut units,
                    CodeKind::Extension,
                    format!("{}/{}#{}", extension.name, file.filename, part.name),
                    format!("gml_Script_{}", part.name),
                    &file.source,
                    Cow::Owned(part.code),
                );
            }
        }
    }

    // `gml_pragma("global", "...")` is removed from its containing unit and
    // compiled as a separate global initializer after every ordinary code
    // unit, matching Class93.list_0 in the official compiler.
    let mut global_sources = Vec::<(&Path, String)>::new();
    for unit in &mut units {
        let (source, globals) = extract_global_pragmas(&unit.code);
        if let Some(source) = source {
            unit.code = Cow::Owned(source);
        }
        global_sources.extend(globals.into_iter().map(|source| (unit.source, source)));
    }
    for (index, (source, code)) in global_sources.into_iter().enumerate() {
        let code = if code.is_empty() {
            " ".to_owned()
        } else {
            code
        };
        push_code(
            &mut units,
            CodeKind::Global,
            format!("global[{index}]"),
            format!("gml_GlobalScript_{index}"),
            source,
            Cow::Owned(code),
        );
    }

    if errors.is_empty() {
        Ok(units)
    } else {
        Err(errors)
    }
}

fn extract_global_pragmas(source: &str) -> (Option<String>, Vec<String>) {
    let Ok(tokens) = lex(source) else {
        return (None, Vec::new());
    };
    let mut ranges = Vec::new();
    let mut globals = Vec::new();
    let mut index = 0;
    while index + 5 < tokens.len() {
        let call = &tokens[index..index + 6];
        let is_global = call[0].kind == TokenKind::Identifier
            && token_text(source, call[0].span) == "gml_pragma"
            && call[1].kind == TokenKind::LeftParen
            && call[2].kind == TokenKind::String
            && string_text(source, call[2].span) == Some("global")
            && call[3].kind == TokenKind::Comma
            && call[4].kind == TokenKind::String
            && call[5].kind == TokenKind::RightParen;
        if is_global {
            if let Some(code) = string_text(source, call[4].span) {
                globals.push(code.to_owned());
                ranges.push(call[0].span.start as usize..call[5].span.end as usize);
            }
            index += 6;
        } else {
            index += 1;
        }
    }
    if ranges.is_empty() {
        return (None, globals);
    }

    let mut stripped = source.as_bytes().to_vec();
    for range in ranges {
        for byte in &mut stripped[range] {
            if *byte != b'\r' && *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    // Replacing every non-newline byte with ASCII spaces preserves UTF-8 and
    // all following byte/line positions.
    (Some(String::from_utf8(stripped).unwrap()), globals)
}

fn token_text(source: &str, span: super::Span) -> &str {
    &source[span.start as usize..span.end as usize]
}

fn string_text(source: &str, span: super::Span) -> Option<&str> {
    let text = token_text(source, span);
    (text.len() >= 2).then(|| &text[1..text.len() - 1])
}

/// Converts the two inline DnD action forms used by the official compiler.
/// Other DnD actions still need library metadata and are handled by the later
/// action lowering pass rather than being guessed here.
pub fn action_code(action: &Action) -> Option<Cow<'_, str>> {
    super::dnd::compiled_code(action)
}

pub fn check_assets(assets: &Assets) -> Result<CheckSummary, Vec<CodeDiagnostic>> {
    let mut units = collect_code(assets).map_err(|errors| {
        errors
            .into_iter()
            .map(|error| CodeDiagnostic {
                kind: error.kind,
                name: error.name,
                source: error.source,
                diagnostic: Diagnostic::new(error.error.to_string(), Default::default()),
            })
            .collect::<Vec<_>>()
    })?;
    let macro_errors = expand_code_macros(assets, &mut units);
    if !macro_errors.is_empty() {
        return Err(macro_errors);
    }
    let results: Vec<_> = units
        .par_iter()
        .map(|unit| match parse(&unit.code) {
            Ok(program) => Ok(program_stats(&program)),
            Err(errors) => Err(errors
                .into_iter()
                .map(|diagnostic| CodeDiagnostic {
                    kind: unit.kind,
                    name: unit.name.clone(),
                    source: unit.source.to_path_buf(),
                    diagnostic,
                })
                .collect::<Vec<_>>()),
        })
        .collect();

    let mut summary = CheckSummary {
        units: units.len(),
        tokens: 0,
        statements: 0,
    };
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok((tokens, statements)) => {
                summary.tokens += tokens;
                summary.statements += statements;
            }
            Err(mut unit_errors) => errors.append(&mut unit_errors),
        }
    }
    if errors.is_empty() {
        Ok(summary)
    } else {
        Err(errors)
    }
}

pub(crate) fn expand_code_macros<'a>(
    assets: &'a Assets,
    units: &mut [CodeUnit<'a>],
) -> Vec<CodeDiagnostic> {
    let table = MacroTable::new(&assets.settings.constants);
    if table.is_empty() {
        return Vec::new();
    }
    let results: Vec<_> = units
        .par_iter()
        .map(|unit| table.expand(&unit.code))
        .collect();
    let mut errors = Vec::new();
    for (unit, result) in units.iter_mut().zip(results) {
        match result {
            Ok(Some(code)) => unit.code = Cow::Owned(code),
            Ok(None) => {}
            Err(diagnostics) => {
                errors.extend(diagnostics.into_iter().map(|diagnostic| CodeDiagnostic {
                    kind: unit.kind,
                    name: unit.name.clone(),
                    source: unit.source.to_path_buf(),
                    diagnostic,
                }))
            }
        }
    }
    errors
}

fn push_code<'a>(
    units: &mut Vec<CodeUnit<'a>>,
    kind: CodeKind,
    name: String,
    vm_name: String,
    source: &'a Path,
    code: Cow<'a, str>,
) -> bool {
    // The official compiler tests String.IsNullOrEmpty, so a whitespace-only
    // script is still a real zero-byte CODE entry with an `arguments` local.
    if !code.is_empty() {
        units.push(CodeUnit {
            kind,
            name,
            vm_name,
            source,
            code,
        });
        true
    } else {
        false
    }
}

const EVENT_NAMES: [&str; 15] = [
    "Create",
    "Destroy",
    "Alarm",
    "Step",
    "Collision",
    "Keyboard",
    "Mouse",
    "Other",
    "Draw",
    "KeyPress",
    "KeyRelease",
    "Trigger",
    "CleanUp",
    "Gesture",
    "PreCreate",
];

fn event_name(event_type: usize) -> &'static str {
    EVENT_NAMES.get(event_type).copied().unwrap_or("Unknown")
}

fn sanitize_name(name: &str) -> String {
    name.replace(' ', "_")
}

fn program_stats(program: &Program) -> (usize, usize) {
    (
        program.token_count,
        program.statements.iter().map(statement_count).sum(),
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::ActionArgument;

    fn action(kind: i32, relative: bool, arguments: &[&str]) -> Action {
        Action {
            library_id: 1,
            id: 0,
            kind,
            use_relative: true,
            is_question: false,
            use_apply_to: true,
            execution_type: 2,
            function_name: String::new(),
            code: String::new(),
            who_name: "self".to_owned(),
            who: -1,
            relative,
            is_not: false,
            arguments: arguments
                .iter()
                .map(|value| ActionArgument {
                    kind: 0,
                    value_kind: String::new(),
                    value: (*value).to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn lowers_inline_dnd_actions() {
        assert_eq!(
            action_code(&action(6, false, &["speed", "4"])).unwrap(),
            "speed = 4;"
        );
        assert_eq!(
            action_code(&action(6, true, &["x", "8"])).unwrap(),
            "x += 8;"
        );
        assert_eq!(
            action_code(&action(7, false, &["show_debug_message(1)"])).unwrap(),
            "show_debug_message(1)"
        );
        assert!(action_code(&action(0, false, &[])).is_none());
    }

    #[test]
    fn extracts_global_pragmas_without_shifting_source_positions() {
        let source = "value = 1;\ngml_pragma(\"global\", 'global.score = 0;');\nvalue += 2;";
        let (stripped, globals) = extract_global_pragmas(source);
        let stripped = stripped.unwrap();
        assert_eq!(globals, ["global.score = 0;"]);
        assert_eq!(stripped.len(), source.len());
        assert_eq!(stripped.matches('\n').count(), 2);
        assert!(!stripped.contains("gml_pragma"));
        assert!(parse(&stripped).is_ok());
    }
}
