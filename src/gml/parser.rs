use super::Diagnostic;
use super::ast::*;
use super::lexer::{Token, TokenKind, lex};

pub fn parse(source: &str) -> Result<Program, Vec<Diagnostic>> {
    let tokens = lex(source)?;
    let token_count = tokens.len().saturating_sub(1);
    let mut parser = Parser {
        tokens,
        position: 0,
    };
    match parser.program(token_count) {
        Ok(program) => Ok(program),
        Err(error) => Err(vec![error]),
    }
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn program(&mut self, token_count: usize) -> Result<Program, Diagnostic> {
        let mut statements = Vec::new();
        while !self.at(TokenKind::Eof) {
            statements.push(self.statement()?);
        }
        Ok(Program {
            statements,
            token_count,
        })
    }

    fn statement(&mut self) -> Result<Stmt, Diagnostic> {
        match self.current().kind {
            TokenKind::Semicolon => {
                let span = self.advance().span;
                Ok(Stmt {
                    kind: StmtKind::Empty,
                    span,
                })
            }
            TokenKind::Begin => self.block(),
            TokenKind::If => self.if_statement(),
            TokenKind::While => self.while_statement(),
            TokenKind::Do => self.do_until_statement(),
            TokenKind::For => self.for_statement(),
            TokenKind::Repeat => self.repeat_statement(),
            TokenKind::With => self.with_statement(),
            TokenKind::Switch => self.switch_statement(),
            TokenKind::Case => self.case_statement(),
            TokenKind::Default => self.default_statement(),
            TokenKind::Var => self.var_statement(false, true),
            TokenKind::GlobalVar => self.var_statement(true, true),
            TokenKind::Enum => self.enum_statement(),
            TokenKind::Return => self.return_statement(),
            TokenKind::Exit => self.simple_statement(StmtKind::Exit),
            TokenKind::Break => self.simple_statement(StmtKind::Break),
            TokenKind::Continue => self.simple_statement(StmtKind::Continue),
            TokenKind::Struct => Err(self.error("struct declarations are not part of GMS 1.4 GML")),
            TokenKind::End => Err(self.error("unexpected end of block")),
            TokenKind::Else => Err(self.error("else has no matching if")),
            TokenKind::Until => Err(self.error("until has no matching do")),
            TokenKind::Eof => Err(self.error("expected a statement")),
            _ => self.expression_statement(),
        }
    }

    fn block(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.expect(TokenKind::Begin, "expected begin or '{'")?.span;
        let mut statements = Vec::new();
        while !self.at(TokenKind::End) {
            if self.at(TokenKind::Eof) {
                return Err(Diagnostic::new("unterminated block", start));
            }
            statements.push(self.statement()?);
        }
        let end = self.advance().span;
        Ok(Stmt {
            kind: StmtKind::Block(statements),
            span: start.through(end),
        })
    }

    fn if_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let condition = self.expression()?;
        self.consume(TokenKind::Then);
        let then_branch = Box::new(self.statement()?);
        let else_branch = if self.consume(TokenKind::Else).is_some() {
            Some(Box::new(self.statement()?))
        } else {
            None
        };
        let end = else_branch
            .as_ref()
            .map_or(then_branch.span, |branch| branch.span);
        Ok(Stmt {
            kind: StmtKind::If {
                condition,
                then_branch,
                else_branch,
            },
            span: start.through(end),
        })
    }

    fn while_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let condition = self.expression()?;
        self.consume(TokenKind::Do);
        let body = Box::new(self.statement()?);
        let span = start.through(body.span);
        Ok(Stmt {
            kind: StmtKind::While { condition, body },
            span,
        })
    }

    fn do_until_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let body = Box::new(self.statement()?);
        self.expect(TokenKind::Until, "expected until after do body")?;
        let condition = self.expression()?;
        let end = self.optional_semicolon(condition.span);
        Ok(Stmt {
            kind: StmtKind::DoUntil { body, condition },
            span: start.through(end),
        })
    }

    fn for_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        self.expect(TokenKind::LeftParen, "expected '(' after for")?;
        let initializer = if self.at(TokenKind::Semicolon) {
            None
        } else if self.at(TokenKind::Var) {
            Some(Box::new(self.var_statement(false, false)?))
        } else if self.at(TokenKind::GlobalVar) {
            Some(Box::new(self.var_statement(true, false)?))
        } else {
            let expression = self.assignment_expression()?;
            let span = expression.span;
            Some(Box::new(Stmt {
                kind: StmtKind::Expr(expression),
                span,
            }))
        };
        self.expect(TokenKind::Semicolon, "expected ';' after for initializer")?;
        let condition = if self.at(TokenKind::Semicolon) {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(TokenKind::Semicolon, "expected ';' after for condition")?;
        let step = if self.at(TokenKind::RightParen) || self.at(TokenKind::Semicolon) {
            None
        } else {
            Some(self.assignment_expression()?)
        };
        while self.consume(TokenKind::Semicolon).is_some() {}
        self.expect(TokenKind::RightParen, "expected ')' after for clauses")?;
        let body = Box::new(self.statement()?);
        let span = start.through(body.span);
        Ok(Stmt {
            kind: StmtKind::For {
                initializer,
                condition,
                step,
                body,
            },
            span,
        })
    }

    fn repeat_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let count = self.expression()?;
        let body = Box::new(self.statement()?);
        let span = start.through(body.span);
        Ok(Stmt {
            kind: StmtKind::Repeat { count, body },
            span,
        })
    }

    fn with_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let target = self.expression()?;
        self.consume(TokenKind::Do);
        let body = Box::new(self.statement()?);
        let span = start.through(body.span);
        Ok(Stmt {
            kind: StmtKind::With { target, body },
            span,
        })
    }

    fn switch_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let value = self.expression()?;
        let body = Box::new(self.statement()?);
        let span = start.through(body.span);
        Ok(Stmt {
            kind: StmtKind::Switch { value, body },
            span,
        })
    }

    fn case_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let value = self.expression()?;
        let colon = self.expect(TokenKind::Colon, "expected ':' after case value")?;
        Ok(Stmt {
            kind: StmtKind::Case(value),
            span: start.through(colon.span),
        })
    }

    fn default_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let colon = self.expect(TokenKind::Colon, "expected ':' after default")?;
        Ok(Stmt {
            kind: StmtKind::Default,
            span: start.through(colon.span),
        })
    }

    fn var_statement(&mut self, global: bool, semicolon: bool) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let mut declarations = Vec::new();
        loop {
            let name = self
                .expect(TokenKind::Identifier, "expected variable name")?
                .span;
            let value = if self.consume(TokenKind::Assign).is_some() {
                Some(self.expression()?)
            } else {
                None
            };
            let end = value.as_ref().map_or(name, |value| value.span);
            declarations.push(VarDecl {
                name,
                value,
                span: name.through(end),
            });
            if self.consume(TokenKind::Comma).is_none() {
                break;
            }
        }
        let mut end = declarations
            .last()
            .map_or(start, |declaration| declaration.span);
        if semicolon {
            end = self.optional_semicolon(end);
        }
        Ok(Stmt {
            kind: StmtKind::Var {
                global,
                declarations,
            },
            span: start.through(end),
        })
    }

    fn enum_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let name = self
            .expect(TokenKind::Identifier, "expected enum name")?
            .span;
        self.expect(TokenKind::Begin, "expected '{' after enum name")?;
        let mut members = Vec::new();
        while !self.at(TokenKind::End) {
            if self.at(TokenKind::Eof) {
                return Err(Diagnostic::new("unterminated enum", start));
            }
            let member_name = self
                .expect(TokenKind::Identifier, "expected enum member name")?
                .span;
            let value = if self.consume(TokenKind::Assign).is_some() {
                Some(self.expression()?)
            } else {
                None
            };
            let end = value.as_ref().map_or(member_name, |value| value.span);
            members.push(EnumMember {
                name: member_name,
                value,
                span: member_name.through(end),
            });
            if self.consume(TokenKind::Comma).is_none() && !self.at(TokenKind::End) {
                return Err(self.error("expected ',' or '}' after enum member"));
            }
        }
        let mut end = self.advance().span;
        end = self.optional_semicolon(end);
        Ok(Stmt {
            kind: StmtKind::Enum { name, members },
            span: start.through(end),
        })
    }

    fn return_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let value = if matches!(
            self.current().kind,
            TokenKind::Semicolon
                | TokenKind::End
                | TokenKind::Eof
                | TokenKind::Else
                | TokenKind::Case
                | TokenKind::Default
                | TokenKind::Until
        ) {
            None
        } else {
            Some(self.expression()?)
        };
        let value_end = value.as_ref().map_or(start, |value| value.span);
        let end = self.optional_semicolon(value_end);
        Ok(Stmt {
            kind: StmtKind::Return(value),
            span: start.through(end),
        })
    }

    fn simple_statement(&mut self, kind: StmtKind) -> Result<Stmt, Diagnostic> {
        let start = self.advance().span;
        let end = self.optional_semicolon(start);
        Ok(Stmt {
            kind,
            span: start.through(end),
        })
    }

    fn expression_statement(&mut self) -> Result<Stmt, Diagnostic> {
        let expression = self.assignment_expression()?;
        let start = expression.span;
        let end = self.optional_semicolon(expression.span);
        Ok(Stmt {
            kind: StmtKind::Expr(expression),
            span: start.through(end),
        })
    }

    fn expression(&mut self) -> Result<Expr, Diagnostic> {
        self.conditional(true)
    }

    fn assignment_expression(&mut self) -> Result<Expr, Diagnostic> {
        let target = self.conditional(false)?;
        let op = match self.current().kind {
            TokenKind::Assign => AssignOp::Set,
            TokenKind::AddAssign => AssignOp::Add,
            TokenKind::SubtractAssign => AssignOp::Subtract,
            TokenKind::MultiplyAssign => AssignOp::Multiply,
            TokenKind::DivideAssign => AssignOp::Divide,
            TokenKind::ModuloAssign => AssignOp::Modulo,
            TokenKind::BitOrAssign => AssignOp::BitOr,
            TokenKind::BitAndAssign => AssignOp::BitAnd,
            TokenKind::BitXorAssign => AssignOp::BitXor,
            _ => return Ok(target),
        };
        self.advance();
        let value = self.assignment_expression()?;
        let span = target.span.through(value.span);
        Ok(Expr {
            kind: ExprKind::Assign {
                op,
                target: Box::new(target),
                value: Box::new(value),
            },
            span,
        })
    }

    fn conditional(&mut self, single_equal: bool) -> Result<Expr, Diagnostic> {
        let condition = self.logical_or(single_equal)?;
        if self.consume(TokenKind::Question).is_none() {
            return Ok(condition);
        }
        let then_value = self.expression()?;
        self.expect(TokenKind::Colon, "expected ':' in conditional expression")?;
        let else_value = self.conditional(single_equal)?;
        let span = condition.span.through(else_value.span);
        Ok(Expr {
            kind: ExprKind::Conditional {
                condition: Box::new(condition),
                then_value: Box::new(then_value),
                else_value: Box::new(else_value),
            },
            span,
        })
    }

    fn logical_or(&mut self, single_equal: bool) -> Result<Expr, Diagnostic> {
        let mut value = self.logical_and(single_equal)?;
        while self.consume(TokenKind::LogicalOr).is_some() {
            let right = self.logical_and(single_equal)?;
            value = binary(BinaryOp::LogicalOr, value, right);
        }
        Ok(value)
    }

    fn logical_and(&mut self, single_equal: bool) -> Result<Expr, Diagnostic> {
        let mut value = self.logical_xor(single_equal)?;
        while self.consume(TokenKind::LogicalAnd).is_some() {
            let right = self.logical_xor(single_equal)?;
            value = binary(BinaryOp::LogicalAnd, value, right);
        }
        Ok(value)
    }

    fn logical_xor(&mut self, single_equal: bool) -> Result<Expr, Diagnostic> {
        let mut value = self.comparison(single_equal)?;
        while self.consume(TokenKind::LogicalXor).is_some() {
            let right = self.comparison(single_equal)?;
            value = binary(BinaryOp::LogicalXor, value, right);
        }
        Ok(value)
    }

    fn comparison(&mut self, single_equal: bool) -> Result<Expr, Diagnostic> {
        let mut value = self.bitwise()?;
        loop {
            let op = match self.current().kind {
                TokenKind::Equal => BinaryOp::Equal,
                TokenKind::Assign if single_equal => BinaryOp::Equal,
                TokenKind::NotEqual => BinaryOp::NotEqual,
                TokenKind::Less => BinaryOp::Less,
                TokenKind::LessEqual => BinaryOp::LessEqual,
                TokenKind::Greater => BinaryOp::Greater,
                TokenKind::GreaterEqual => BinaryOp::GreaterEqual,
                _ => break,
            };
            self.advance();
            let right = self.bitwise()?;
            value = binary(op, value, right);
        }
        Ok(value)
    }

    fn bitwise(&mut self) -> Result<Expr, Diagnostic> {
        let mut value = self.shift()?;
        loop {
            let op = match self.current().kind {
                TokenKind::BitOr => BinaryOp::BitOr,
                TokenKind::BitAnd => BinaryOp::BitAnd,
                TokenKind::BitXor => BinaryOp::BitXor,
                _ => break,
            };
            self.advance();
            let right = self.shift()?;
            value = binary(op, value, right);
        }
        Ok(value)
    }

    fn shift(&mut self) -> Result<Expr, Diagnostic> {
        let mut value = self.additive()?;
        loop {
            let op = match self.current().kind {
                TokenKind::ShiftLeft => BinaryOp::ShiftLeft,
                TokenKind::ShiftRight => BinaryOp::ShiftRight,
                _ => break,
            };
            self.advance();
            let right = self.additive()?;
            value = binary(op, value, right);
        }
        Ok(value)
    }

    fn additive(&mut self) -> Result<Expr, Diagnostic> {
        let mut value = self.multiplicative()?;
        loop {
            let op = match self.current().kind {
                TokenKind::Add => BinaryOp::Add,
                TokenKind::Subtract => BinaryOp::Subtract,
                _ => break,
            };
            self.advance();
            let right = self.multiplicative()?;
            value = binary(op, value, right);
        }
        Ok(value)
    }

    fn multiplicative(&mut self) -> Result<Expr, Diagnostic> {
        let mut value = self.unary()?;
        loop {
            let op = match self.current().kind {
                TokenKind::Multiply => BinaryOp::Multiply,
                TokenKind::Divide => BinaryOp::Divide,
                TokenKind::IntegerDivide => BinaryOp::IntegerDivide,
                TokenKind::Modulo => BinaryOp::Modulo,
                _ => break,
            };
            self.advance();
            let right = self.unary()?;
            value = binary(op, value, right);
        }
        Ok(value)
    }

    fn unary(&mut self) -> Result<Expr, Diagnostic> {
        let op = match self.current().kind {
            TokenKind::Add => UnaryOp::Positive,
            TokenKind::Subtract => UnaryOp::Negative,
            TokenKind::Not => UnaryOp::Not,
            TokenKind::BitNot => UnaryOp::BitNot,
            TokenKind::Increment => UnaryOp::PreIncrement,
            TokenKind::Decrement => UnaryOp::PreDecrement,
            _ => return self.postfix(),
        };
        let start = self.advance().span;
        let value = self.unary()?;
        let span = start.through(value.span);
        Ok(Expr {
            kind: ExprKind::Unary {
                op,
                value: Box::new(value),
            },
            span,
        })
    }

    fn postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut value = self.primary()?;
        loop {
            match self.current().kind {
                TokenKind::LeftParen => {
                    self.advance();
                    let mut arguments = Vec::new();
                    if !self.at(TokenKind::RightParen) {
                        loop {
                            arguments.push(self.expression()?);
                            if self.consume(TokenKind::Comma).is_none() {
                                break;
                            }
                            if self.at(TokenKind::RightParen) {
                                break;
                            }
                        }
                    }
                    let end = self
                        .expect(TokenKind::RightParen, "expected ')' after arguments")?
                        .span;
                    let span = value.span.through(end);
                    value = Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(value),
                            arguments,
                        },
                        span,
                    };
                }
                TokenKind::AccessorOpen(accessor) => {
                    self.advance();
                    let mut indices = Vec::new();
                    if self.at(TokenKind::RightBracket) {
                        return Err(self.error("array access requires an index"));
                    }
                    loop {
                        indices.push(self.expression()?);
                        if self.consume(TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                    let end = self
                        .expect(TokenKind::RightBracket, "expected ']' after index")?
                        .span;
                    let span = value.span.through(end);
                    value = Expr {
                        kind: ExprKind::Index {
                            target: Box::new(value),
                            accessor,
                            indices,
                        },
                        span,
                    };
                }
                TokenKind::Dot => {
                    self.advance();
                    let name = self
                        .expect(TokenKind::Identifier, "expected member name after '.'")?
                        .span;
                    let span = value.span.through(name);
                    value = Expr {
                        kind: ExprKind::Member {
                            target: Box::new(value),
                            name,
                        },
                        span,
                    };
                }
                TokenKind::Increment | TokenKind::Decrement => {
                    let token = self.advance();
                    let op = if token.kind == TokenKind::Increment {
                        PostfixOp::Increment
                    } else {
                        PostfixOp::Decrement
                    };
                    let span = value.span.through(token.span);
                    value = Expr {
                        kind: ExprKind::Postfix {
                            op,
                            target: Box::new(value),
                        },
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(value)
    }

    fn primary(&mut self) -> Result<Expr, Diagnostic> {
        let token = self.current();
        match token.kind {
            TokenKind::Identifier => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Identifier,
                    span: token.span,
                })
            }
            TokenKind::Number => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::Number,
                    span: token.span,
                })
            }
            TokenKind::String => {
                self.advance();
                Ok(Expr {
                    kind: ExprKind::String,
                    span: token.span,
                })
            }
            TokenKind::LeftParen => {
                let start = self.advance().span;
                let value = self.expression()?;
                let end = self
                    .expect(TokenKind::RightParen, "expected ')' after expression")?
                    .span;
                Ok(Expr {
                    kind: ExprKind::Group(Box::new(value)),
                    span: start.through(end),
                })
            }
            TokenKind::AccessorOpen(Accessor::Array) => {
                let start = self.advance().span;
                let mut values = Vec::new();
                if !self.at(TokenKind::RightBracket) {
                    loop {
                        values.push(self.expression()?);
                        if self.consume(TokenKind::Comma).is_none() {
                            break;
                        }
                        if self.at(TokenKind::RightBracket) {
                            break;
                        }
                    }
                }
                let end = self
                    .expect(TokenKind::RightBracket, "expected ']' after array literal")?
                    .span;
                Ok(Expr {
                    kind: ExprKind::Array(values),
                    span: start.through(end),
                })
            }
            TokenKind::AccessorOpen(_) => Err(self.error("typed accessor requires a target")),
            _ => Err(self.error(format!(
                "expected expression, found {}",
                token_name(token.kind)
            ))),
        }
    }

    fn optional_semicolon(&mut self, fallback: Span) -> Span {
        self.consume(TokenKind::Semicolon)
            .map_or(fallback, |token| token.span)
    }

    fn current(&self) -> Token {
        self.tokens[self.position]
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.current().kind == kind
    }

    fn advance(&mut self) -> Token {
        let token = self.current();
        if token.kind != TokenKind::Eof {
            self.position += 1;
        }
        token
    }

    fn consume(&mut self, kind: TokenKind) -> Option<Token> {
        self.at(kind).then(|| self.advance())
    }

    fn expect(&mut self, kind: TokenKind, message: &str) -> Result<Token, Diagnostic> {
        if self.at(kind) {
            Ok(self.advance())
        } else {
            Err(self.error(message))
        }
    }

    fn error(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(message, self.current().span)
    }
}

fn binary(op: BinaryOp, left: Expr, right: Expr) -> Expr {
    let span = left.span.through(right.span);
    Expr {
        kind: ExprKind::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        },
        span,
    }
}

fn token_name(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Eof => "end of file",
        TokenKind::Identifier => "identifier",
        TokenKind::Number => "number",
        TokenKind::String => "string",
        TokenKind::Begin => "begin or '{'",
        TokenKind::End => "end or '}'",
        TokenKind::If => "if",
        TokenKind::Then => "then",
        TokenKind::Else => "else",
        TokenKind::While => "while",
        TokenKind::Do => "do",
        TokenKind::For => "for",
        TokenKind::Until => "until",
        TokenKind::Repeat => "repeat",
        TokenKind::Exit => "exit",
        TokenKind::Break => "break",
        TokenKind::Continue => "continue",
        TokenKind::With => "with",
        TokenKind::Return => "return",
        TokenKind::Switch => "switch",
        TokenKind::Case => "case",
        TokenKind::Default => "default",
        TokenKind::Var => "var",
        TokenKind::GlobalVar => "globalvar",
        TokenKind::Enum => "enum",
        TokenKind::Struct => "struct",
        TokenKind::LeftParen => "'('",
        TokenKind::RightParen => "')'",
        TokenKind::AccessorOpen(_) => "'['",
        TokenKind::RightBracket => "']'",
        TokenKind::Semicolon => "';'",
        TokenKind::Comma => "','",
        TokenKind::Dot => "'.'",
        TokenKind::Colon => "':'",
        TokenKind::Question => "'?'",
        TokenKind::Assign => "'='",
        TokenKind::AddAssign => "'+='",
        TokenKind::SubtractAssign => "'-='",
        TokenKind::MultiplyAssign => "'*='",
        TokenKind::DivideAssign => "'/='",
        TokenKind::ModuloAssign => "'%='",
        TokenKind::BitOrAssign => "'|='",
        TokenKind::BitAndAssign => "'&='",
        TokenKind::BitXorAssign => "'^='",
        TokenKind::LogicalOr => "logical or",
        TokenKind::LogicalAnd => "logical and",
        TokenKind::LogicalXor => "logical xor",
        TokenKind::Not => "not",
        TokenKind::Equal => "'=='",
        TokenKind::NotEqual => "'!='",
        TokenKind::Less => "'<'",
        TokenKind::LessEqual => "'<='",
        TokenKind::Greater => "'>'",
        TokenKind::GreaterEqual => "'>='",
        TokenKind::BitOr => "'|'",
        TokenKind::BitAnd => "'&'",
        TokenKind::BitXor => "'^'",
        TokenKind::BitNot => "'~'",
        TokenKind::ShiftLeft => "'<<'",
        TokenKind::ShiftRight => "'>>'",
        TokenKind::Add => "'+'",
        TokenKind::Subtract => "'-'",
        TokenKind::Multiply => "'*'",
        TokenKind::Divide => "'/'",
        TokenKind::IntegerDivide => "div",
        TokenKind::Modulo => "mod",
        TokenKind::Increment => "'++'",
        TokenKind::Decrement => "'--'",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_control_flow() {
        let source = r#"
            var i = 0, total = 0;
            for (i = 0; i < 10; i++) begin
                if i mod 2 = 0 then total += i else continue;
            end
            do total-- until total <= 0;
            return total;
        "#;
        let program = parse(source).unwrap();
        assert_eq!(program.statements.len(), 4);
        assert!(matches!(program.statements[1].kind, StmtKind::For { .. }));
        assert!(matches!(
            program.statements[2].kind,
            StmtKind::DoUntil { .. }
        ));
    }

    #[test]
    fn accepts_trailing_for_clause_semicolons_like_the_official_parser() {
        let program = parse("for (i = 0; i < 10; i += 1;) value += i; for (;;;) break;").unwrap();

        assert_eq!(program.statements.len(), 2);
        let StmtKind::For { step, .. } = &program.statements[0].kind else {
            panic!("first statement should be a for loop");
        };
        assert!(step.is_some());
        let StmtKind::For {
            condition, step, ..
        } = &program.statements[1].kind
        else {
            panic!("second statement should be a for loop");
        };
        assert!(condition.is_none());
        assert!(step.is_none());
    }

    #[test]
    fn distinguishes_statement_assignment_from_single_equal_comparison() {
        let program = parse("a = 1; if (a = 1) b = 2;").unwrap();
        let StmtKind::Expr(Expr {
            kind: ExprKind::Assign { .. },
            ..
        }) = &program.statements[0].kind
        else {
            panic!("first statement should be an assignment");
        };
        let StmtKind::If { condition, .. } = &program.statements[1].kind else {
            panic!("second statement should be an if");
        };
        let ExprKind::Group(condition) = &condition.kind else {
            panic!("condition should preserve grouping");
        };
        assert!(matches!(
            condition.kind,
            ExprKind::Binary {
                op: BinaryOp::Equal,
                ..
            }
        ));
    }

    #[test]
    fn parses_enum_switch_and_accessors() {
        let source = r#"
            enum State { Idle, Run = 4, Done }
            switch state {
                case State.Run: value = map[? "key"];
                default: value = grid[# x, y]; break;
            }
        "#;
        let program = parse(source).unwrap();
        assert!(matches!(program.statements[0].kind, StmtKind::Enum { .. }));
        assert!(matches!(
            program.statements[1].kind,
            StmtKind::Switch { .. }
        ));
    }

    #[test]
    fn reports_line_and_column() {
        let errors = parse("if true {\n  value = ;\n}").unwrap_err();
        assert_eq!(errors[0].span.line, 2);
        assert_eq!(errors[0].span.column, 11);
    }
}
