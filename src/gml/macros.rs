use std::collections::HashMap;

use crate::settings::CompileConstant;

use super::{Diagnostic, Span, TokenKind, lex};

/// The project, configuration, and extension constants consumed by the GMS
/// 1.4 compiler are token macros rather than immutable values. In particular,
/// an extension may map a public name to a writable global variable.
pub(crate) struct MacroTable<'a> {
    definitions: HashMap<&'a str, &'a str>,
}

impl<'a> MacroTable<'a> {
    pub(crate) fn new(constants: &'a [CompileConstant]) -> Self {
        Self {
            definitions: constants
                .iter()
                .map(|constant| (constant.name.as_str(), constant.value.as_str()))
                .collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /// Expands macro tokens while retaining the ordinary source text around
    /// them. `None` means that no macro occurred and lets callers avoid an
    /// allocation for the common case.
    pub(crate) fn expand(&self, source: &str) -> Result<Option<String>, Vec<Diagnostic>> {
        if self.is_empty() {
            return Ok(None);
        }
        let tokens = lex(source)?;
        let mut expander = Expander::new(self);
        let mut output = String::new();
        let mut cursor = 0_usize;
        let mut previous_kind = None;
        let mut changed = false;

        for (index, token) in tokens[..tokens.len() - 1].iter().enumerate() {
            let text = span_text(source, token.span);
            let Some((name, _)) = self.definitions.get_key_value(text) else {
                previous_kind = Some(token.kind);
                continue;
            };
            if token.kind != TokenKind::Identifier {
                previous_kind = Some(token.kind);
                continue;
            }
            if previous_kind == Some(TokenKind::Dot) {
                return Err(vec![Diagnostic::new(
                    format!("\"{name}\" is a macro and cannot be used as an instance variable"),
                    token.span,
                )]);
            }

            let replacement = expander.expand_macro(name).map_err(|message| {
                vec![Diagnostic::new(
                    format!("cannot expand macro {name}: {message}"),
                    token.span,
                )]
            })?;
            if tokens.get(index + 1).map(|next| next.kind) == Some(TokenKind::LeftParen)
                && !replacement.single_identifier()
            {
                return Err(vec![Diagnostic::new(
                    format!("malformed macro used as function name {name}"),
                    token.span,
                )]);
            }

            output.push_str(&source[cursor..token.span.start as usize]);
            if !replacement.text.is_empty() {
                output.push(' ');
                output.push_str(&replacement.text);
                output.push(' ');
            }
            cursor = token.span.end as usize;
            previous_kind = replacement.last_kind.or(previous_kind);
            changed = true;
        }

        if !changed {
            return Ok(None);
        }
        output.push_str(&source[cursor..]);
        Ok(Some(output))
    }
}

#[derive(Clone)]
struct ExpandedMacro {
    text: String,
    token_count: usize,
    first_kind: Option<TokenKind>,
    last_kind: Option<TokenKind>,
}

impl ExpandedMacro {
    fn single_identifier(&self) -> bool {
        self.token_count == 1 && self.first_kind == Some(TokenKind::Identifier)
    }
}

struct Expander<'table, 'definition> {
    table: &'table MacroTable<'definition>,
    cache: HashMap<&'definition str, ExpandedMacro>,
    stack: Vec<&'definition str>,
}

impl<'table, 'definition> Expander<'table, 'definition> {
    fn new(table: &'table MacroTable<'definition>) -> Self {
        Self {
            table,
            cache: HashMap::new(),
            stack: Vec::new(),
        }
    }

    fn expand_macro(&mut self, name: &str) -> Result<ExpandedMacro, String> {
        let Some((&definition_name, &value)) = self.table.definitions.get_key_value(name) else {
            unreachable!("macro names are looked up before expansion")
        };
        if let Some(expanded) = self.cache.get(definition_name) {
            return Ok(expanded.clone());
        }
        if self.stack.contains(&definition_name) {
            let mut chain = self.stack.join(" -> ");
            if !chain.is_empty() {
                chain.push_str(" -> ");
            }
            chain.push_str(definition_name);
            return Err(format!(
                "recursive macro expansion is not supported ({chain})"
            ));
        }
        if value.trim().is_empty() {
            return Err("macro has an empty value".to_owned());
        }

        self.stack.push(definition_name);
        let expanded = self
            .expand_fragment(value)
            .map_err(|message| format!("invalid definition of {definition_name}: {message}"));
        self.stack.pop();
        let expanded = expanded?;
        self.cache.insert(definition_name, expanded.clone());
        Ok(expanded)
    }

    fn expand_fragment(&mut self, source: &str) -> Result<ExpandedMacro, String> {
        let tokens = lex(source).map_err(|errors| {
            errors
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        let mut text = String::new();
        let mut token_count = 0_usize;
        let mut first_kind = None;
        let mut last_kind = None;

        for (index, token) in tokens[..tokens.len() - 1].iter().enumerate() {
            let token_text = span_text(source, token.span);
            let replacement = if token.kind == TokenKind::Identifier
                && self.table.definitions.contains_key(token_text)
            {
                if last_kind == Some(TokenKind::Dot) {
                    return Err(format!(
                        "\"{token_text}\" is a macro and cannot be used as an instance variable"
                    ));
                }
                let replacement = self.expand_macro(token_text)?;
                if tokens.get(index + 1).map(|next| next.kind) == Some(TokenKind::LeftParen)
                    && !replacement.single_identifier()
                {
                    return Err(format!(
                        "malformed macro used as function name {token_text}"
                    ));
                }
                replacement
            } else {
                ExpandedMacro {
                    text: token_text.to_owned(),
                    token_count: 1,
                    first_kind: Some(token.kind),
                    last_kind: Some(token.kind),
                }
            };

            if !replacement.text.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&replacement.text);
            }
            if first_kind.is_none() {
                first_kind = replacement.first_kind;
            }
            if replacement.last_kind.is_some() {
                last_kind = replacement.last_kind;
            }
            token_count = token_count.saturating_add(replacement.token_count);
        }

        Ok(ExpandedMacro {
            text,
            token_count,
            first_kind,
            last_kind,
        })
    }
}

fn span_text(source: &str, span: Span) -> &str {
    &source[span.start as usize..span.end as usize]
}

#[cfg(test)]
mod tests {
    use crate::settings::ConstantSource;

    use super::*;

    fn constant(name: &str, value: &str) -> CompileConstant {
        CompileConstant {
            name: name.to_owned(),
            value: value.to_owned(),
            source: ConstantSource::Extension,
        }
    }

    #[test]
    fn expands_expression_and_writable_global_macros() {
        let constants = [
            constant("LIMIT", "40 + 2"),
            constant("nsfs_is_available", "global.g_nsfs_is_available"),
        ];
        let table = MacroTable::new(&constants);
        let source = "nsfs_is_available = LIMIT; return nsfs_is_available; // nsfs_is_available";
        let expanded = table.expand(source).unwrap().unwrap();
        let normalized = expanded.split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(normalized.contains("global . g_nsfs_is_available = 40 + 2"));
        assert!(normalized.contains("return global . g_nsfs_is_available"));
        assert!(expanded.ends_with("// nsfs_is_available"));
    }

    #[test]
    fn expands_forward_references_and_function_aliases() {
        let constants = [
            constant("FIRST", "SECOND"),
            constant("SECOND", "1"),
            constant("TRACE", "show_debug_message"),
        ];
        let table = MacroTable::new(&constants);
        let expanded = table.expand("TRACE(FIRST);").unwrap().unwrap();

        assert!(expanded.contains("show_debug_message"));
        assert!(expanded.contains("( 1 )"));
    }

    #[test]
    fn rejects_recursive_and_instance_member_macro_uses() {
        let constants = [constant("FIRST", "SECOND"), constant("SECOND", "FIRST")];
        let table = MacroTable::new(&constants);
        let error = table.expand("return FIRST;").unwrap_err();
        assert!(error[0].message.contains("recursive macro expansion"));

        let constants = [constant("value", "global.actual")];
        let table = MacroTable::new(&constants);
        let error = table.expand("holder.value = 1;").unwrap_err();
        assert!(
            error[0]
                .message
                .contains("cannot be used as an instance variable")
        );
    }
}
