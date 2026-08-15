use std::borrow::Cow;
use std::error::Error;
use std::fmt::{self, Write};

use crate::assets::{Action, ActionArgument};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DndError {
    pub action: usize,
    pub message: String,
}

impl fmt::Display for DndError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "action {}: {}", self.action, self.message)
    }
}

impl Error for DndError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedScript {
    pub name: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredActions {
    pub code: String,
    pub scripts: Vec<GeneratedScript>,
}

#[derive(Debug, Default)]
pub struct DndContext {
    next_script: usize,
}

impl DndContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lower(&mut self, actions: &[Action]) -> Result<LoweredActions, DndError> {
        Lowerer::new(actions, self).run()
    }

    fn script_name(&mut self) -> String {
        let name = format!("__script{}__", self.next_script);
        self.next_script += 1;
        name
    }
}

/// Lowers one standalone GMS 1.4 DnD action list. Project compilation should
/// reuse a `DndContext` so generated question-script names remain unique.
pub fn lower_actions(actions: &[Action]) -> Result<LoweredActions, DndError> {
    DndContext::new().lower(actions)
}

struct Lowerer<'a, 'context> {
    actions: &'a [Action],
    context: &'context mut DndContext,
    output: String,
    scripts: Vec<GeneratedScript>,
    relative: Vec<bool>,
    condition_variable: bool,
}

impl<'a, 'context> Lowerer<'a, 'context> {
    fn new(actions: &'a [Action], context: &'context mut DndContext) -> Self {
        Self {
            actions,
            context,
            output: String::with_capacity(actions.len() * 48 + 16),
            scripts: Vec::new(),
            relative: Vec::new(),
            condition_variable: false,
        }
    }

    fn run(mut self) -> Result<LoweredActions, DndError> {
        let mut current_relative = false;
        let mut transitions = 0;
        for action in self.actions {
            if action.use_relative {
                if current_relative != action.relative {
                    transitions += 1;
                }
                current_relative = action.relative;
            }
        }
        if transitions != 0
            && let Some(action) = self.actions.iter().find(|action| action.use_relative)
        {
            self.relative.push(action.relative);
        }

        self.output.push_str("{\n");
        if let Some(relative) = self.relative.last() {
            writeln!(
                self.output,
                "action_set_relative( {} );",
                i32::from(*relative)
            )
            .unwrap();
        }
        let mut index = 0;
        while index < self.actions.len() {
            if self.actions[index].function_name == "action_execute_script" {
                index = self.execute_script(index)?;
            } else {
                index = self.action(index)?;
            }
        }
        if !self.relative.is_empty() {
            self.output.push_str("action_set_relative( 0 );\n");
            self.relative.pop();
        }
        if !self.relative.is_empty() {
            return Err(self.error(index.saturating_sub(1), "unbalanced relative state"));
        }
        self.output.push_str("/*  */ }\n");
        Ok(LoweredActions {
            code: self.output,
            scripts: self.scripts,
        })
    }

    fn action(&mut self, index: usize) -> Result<usize, DndError> {
        let action = &self.actions[index];
        match action.kind {
            1 => self.block(index),
            2 => Err(self.error(index, "end action has no matching begin")),
            3 => Err(self.error(index, "else action has no matching question")),
            4 => {
                if !self.relative.is_empty() {
                    self.output.push_str("action_set_relative( 0 );\n");
                }
                self.output.push_str("exit;\n");
                Ok(index + 1)
            }
            5 => self.repeat(index),
            _ if action.is_question => self.condition(index),
            _ => self.normal(index),
        }
    }

    fn block(&mut self, index: usize) -> Result<usize, DndError> {
        self.output.push_str("{\n");
        let mut next = index + 1;
        while next < self.actions.len() {
            if self.actions[next].kind == 2 {
                self.output.push_str("}\n");
                return Ok(next + 1);
            }
            next = self.action(next)?;
        }
        Err(self.error(index, "begin action has no matching end"))
    }

    fn repeat(&mut self, index: usize) -> Result<usize, DndError> {
        let count = self.actions[index]
            .arguments
            .first()
            .ok_or_else(|| self.error(index, "repeat action requires a count"))?;
        writeln!(self.output, "repeat( {} )", count.value).unwrap();
        if index + 1 >= self.actions.len() {
            return Err(self.error(index, "repeat action has no body"));
        }
        self.action(index + 1)
    }

    fn condition(&mut self, index: usize) -> Result<usize, DndError> {
        let action = &self.actions[index];
        let (apply_block, break_from_apply) = self.apply_to(action);
        let relative_block = self.relative_differs(action);
        let wrapper = apply_block || relative_block;
        if wrapper {
            self.output.push_str("{\n");
        }
        if !self.condition_variable {
            self.output.push_str("var __b__;\n");
            self.condition_variable = true;
        }
        let restore_relative = self.push_relative(action);
        self.condition_expression(index)?;
        if break_from_apply {
            writeln!(
                self.output,
                "if {}__b__ break;",
                if action.is_not { "!" } else { "" }
            )
            .unwrap();
        }
        if restore_relative {
            self.pop_relative(index)?;
        }
        if wrapper {
            self.output.push_str("}\n");
        }
        writeln!(
            self.output,
            "if {}__b__",
            if action.is_not { "!" } else { "" }
        )
        .unwrap();
        self.output.push_str("{\n");
        if index + 1 >= self.actions.len() {
            return Err(self.error(index, "question action has no following action"));
        }
        let mut next = self.action(index + 1)?;
        self.output.push_str("}\n");
        if next < self.actions.len() && self.actions[next].kind == 3 {
            self.output.push_str("else\n{\n");
            if next + 1 >= self.actions.len() {
                return Err(self.error(next, "else action has no following action"));
            }
            next = self.action(next + 1)?;
            self.output.push_str("}\n");
        }
        Ok(next)
    }

    fn normal(&mut self, index: usize) -> Result<usize, DndError> {
        let action = &self.actions[index];
        if action.execution_type == 0 && !matches!(action.kind, 6 | 7) {
            return Ok(index + 1);
        }
        let (apply_block, _) = self.apply_to(action);
        let wrapper = apply_block || self.relative_differs(action);
        if wrapper {
            self.output.push_str("{\n");
        }
        let restore_relative = self.push_relative(action);
        self.normal_expression(index)?;
        if restore_relative {
            self.pop_relative(index)?;
        }
        if wrapper {
            self.output.push_str("}\n");
        }
        Ok(index + 1)
    }

    fn condition_expression(&mut self, index: usize) -> Result<(), DndError> {
        let action = &self.actions[index];
        match action.execution_type {
            1 if action.function_name == "action_execute_script" => {
                self.output.push_str("__b__ = ");
                self.execute_script_call(index)?;
            }
            1 => {
                write!(self.output, "__b__ = {}( ", action.function_name).unwrap();
                self.function_arguments(action);
                self.output.push_str(" );\n");
            }
            2 => {
                let code = compiled_code(action).ok_or_else(|| {
                    self.error(index, "code action does not contain executable code")
                })?;
                let name = self.context.script_name();
                self.scripts.push(GeneratedScript {
                    name: name.clone(),
                    code: code.into_owned(),
                });
                write!(self.output, "__b__ = {name}( ").unwrap();
                if !matches!(action.kind, 6 | 7) {
                    self.function_arguments(action);
                }
                self.output.push_str(" );\n");
            }
            _ => {
                return Err(self.error(index, "question action has no executable function or code"));
            }
        }
        Ok(())
    }

    fn normal_expression(&mut self, index: usize) -> Result<(), DndError> {
        let action = &self.actions[index];
        match action.execution_type {
            1 if action.function_name == "action_execute_script" => {
                self.execute_script_call(index)?;
            }
            1 => {
                write!(self.output, "{}( ", action.function_name).unwrap();
                self.function_arguments(action);
                self.output.push_str(" );\n");
            }
            2 => {
                let mut code = compiled_code(action)
                    .ok_or_else(|| self.error(index, "code action does not contain code"))?
                    .into_owned();
                if !matches!(action.kind, 6 | 7) {
                    for (argument_index, argument) in action.arguments.iter().enumerate() {
                        let value = code_argument(argument);
                        code = code.replace(&format!("argument{argument_index}"), &value);
                    }
                }
                self.output.push_str(&code);
                self.output.push('\n');
                self.output.push_str("/* */\n");
            }
            0 => {}
            _ => return Err(self.error(index, "unknown action execution type")),
        }
        Ok(())
    }

    fn function_arguments(&mut self, action: &Action) {
        for (index, argument) in action.arguments.iter().enumerate() {
            if index != 0 {
                self.output.push_str(", ");
            }
            self.output.push_str(&function_argument(argument));
        }
    }

    fn execute_script(&mut self, index: usize) -> Result<usize, DndError> {
        self.execute_script_call(index)?;
        Ok(index + 1)
    }

    fn execute_script_call(&mut self, index: usize) -> Result<(), DndError> {
        let action = &self.actions[index];
        if !matches!(action.arguments.len(), 6 | 8) {
            return Err(self.error(
                index,
                "action_execute_script requires six or eight arguments",
            ));
        }
        let script = action.arguments[0].value.trim();
        if script.is_empty() {
            return Ok(());
        }
        if action.use_apply_to {
            match action.who {
                -1 => {}
                -2 => self.output.push_str("with( other )\n"),
                who => writeln!(self.output, "with( {who} )").unwrap(),
            }
        }
        write!(self.output, "{script}(").unwrap();
        for index in 1..6 {
            let argument = action.arguments[index].value.trim();
            if !argument.is_empty() {
                if index >= 2 {
                    self.output.push(',');
                }
                self.output.push_str(argument);
            }
        }
        self.output.push_str(");\n");
        Ok(())
    }

    /// Emits an apply-to prefix and returns (needs wrapper, needs break bridge).
    fn apply_to(&mut self, action: &Action) -> (bool, bool) {
        if !action.use_apply_to {
            return (false, false);
        }
        match action.who {
            -1 => (false, false),
            -2 => {
                self.output.push_str("with( other )\n");
                (true, false)
            }
            who => {
                writeln!(self.output, "with( {who} )").unwrap();
                (true, true)
            }
        }
    }

    fn relative_differs(&self, action: &Action) -> bool {
        action.use_relative
            && self
                .relative
                .last()
                .is_some_and(|relative| *relative != action.relative)
    }

    fn push_relative(&mut self, action: &Action) -> bool {
        if self.relative_differs(action) {
            writeln!(
                self.output,
                "action_set_relative( {} );",
                i32::from(action.relative)
            )
            .unwrap();
            self.relative.push(action.relative);
            true
        } else {
            false
        }
    }

    fn pop_relative(&mut self, index: usize) -> Result<(), DndError> {
        let previous = self
            .relative
            .pop()
            .ok_or_else(|| self.error(index, "relative state stack underflow"))?;
        let restored = self
            .relative
            .last()
            .copied()
            .ok_or_else(|| self.error(index, "relative base state is missing"))?;
        if previous != restored {
            writeln!(
                self.output,
                "action_set_relative( {} );",
                i32::from(restored)
            )
            .unwrap();
        }
        Ok(())
    }

    fn error(&self, action: usize, message: impl Into<String>) -> DndError {
        DndError {
            action,
            message: message.into(),
        }
    }
}

pub(super) fn compiled_code(action: &Action) -> Option<Cow<'_, str>> {
    match action.kind {
        6 => {
            let variable = action.arguments.first()?.value.trim();
            let value = action.arguments.get(1)?.value.trim();
            if variable.is_empty() || value.is_empty() {
                return None;
            }
            let operator = if action.relative { "+=" } else { "=" };
            Some(Cow::Owned(format!(
                "{}{variable} {operator} {value};",
                action.code
            )))
        }
        7 => action
            .arguments
            .first()
            .map(|argument| Cow::Borrowed(argument.value.as_str()))
            .or_else(|| (!action.code.is_empty()).then_some(Cow::Borrowed(action.code.as_str()))),
        _ if !action.code.is_empty() => Some(Cow::Borrowed(action.code.as_str())),
        _ => None,
    }
}

fn function_argument(argument: &ActionArgument) -> Cow<'_, str> {
    if matches!(argument.kind, 1 | 2)
        && !argument.value.starts_with('\'')
        && !argument.value.starts_with('"')
    {
        Cow::Owned(format!("\"{}\"", argument.value))
    } else {
        Cow::Borrowed(&argument.value)
    }
}

fn code_argument(argument: &ActionArgument) -> Cow<'_, str> {
    if argument.kind == 1 && (!argument.value.starts_with('"') || !argument.value.ends_with('"')) {
        Cow::Owned(format!("\"{}\"", argument.value))
    } else {
        Cow::Borrowed(&argument.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(kind: i32, execution_type: i32, function: &str, arguments: &[(&str, i32)]) -> Action {
        Action {
            library_id: 1,
            id: 0,
            kind,
            use_relative: false,
            is_question: false,
            use_apply_to: false,
            execution_type,
            function_name: function.to_owned(),
            code: String::new(),
            who_name: "self".to_owned(),
            who: -1,
            relative: false,
            is_not: false,
            arguments: arguments
                .iter()
                .map(|(value, kind)| ActionArgument {
                    kind: *kind,
                    value_kind: String::new(),
                    value: (*value).to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn lowers_function_variable_and_code_actions() {
        let actions = [
            action(0, 1, "show_debug_message", &[("hello", 1)]),
            action(6, 2, "", &[("score", 1), ("10", 0)]),
            action(7, 2, "", &[("x += 1;", 1)]),
        ];
        let lowered = lower_actions(&actions).unwrap();
        assert!(lowered.code.contains("show_debug_message( \"hello\" );"));
        assert!(lowered.code.contains("score = 10;"));
        assert!(lowered.code.contains("x += 1;"));
        assert!(super::super::parse(&lowered.code).is_ok());
    }

    #[test]
    fn lowers_question_block_else_and_repeat() {
        let mut question = action(
            0,
            1,
            "action_if_variable",
            &[("score", 0), ("10", 0), ("0", 4)],
        );
        question.is_question = true;
        let actions = [
            question,
            action(1, 0, "", &[]),
            action(7, 2, "", &[("score += 1;", 1)]),
            action(2, 0, "", &[]),
            action(3, 0, "", &[]),
            action(5, 0, "", &[("2", 0)]),
            action(7, 2, "", &[("score -= 1;", 1)]),
        ];
        let lowered = lower_actions(&actions).unwrap();
        assert!(lowered.code.contains("var __b__;"));
        assert!(lowered.code.contains("else"));
        assert!(lowered.code.contains("repeat( 2 )"));
        assert!(super::super::parse(&lowered.code).is_ok());
    }

    #[test]
    fn rejects_unbalanced_control_actions() {
        let error = lower_actions(&[action(1, 0, "", &[])]).unwrap_err();
        assert!(error.message.contains("matching end"));
    }

    #[test]
    fn turns_question_code_into_a_generated_script() {
        let mut question = action(7, 2, "", &[("return score > 0;", 1)]);
        question.is_question = true;
        let lowered = lower_actions(&[question, action(7, 2, "", &[("score = 0;", 1)])]).unwrap();
        assert_eq!(lowered.scripts.len(), 1);
        assert_eq!(lowered.scripts[0].name, "__script0__");
        assert_eq!(lowered.scripts[0].code, "return score > 0;");
        assert!(lowered.code.contains("__b__ = __script0__(  );"));
    }
}
