use super::Diagnostic;
use super::ast::{Accessor, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Eof,
    Identifier,
    Number,
    String,
    Begin,
    End,
    If,
    Then,
    Else,
    While,
    Do,
    For,
    Until,
    Repeat,
    Exit,
    Break,
    Continue,
    With,
    Return,
    Switch,
    Case,
    Default,
    Var,
    GlobalVar,
    Enum,
    Struct,
    LeftParen,
    RightParen,
    AccessorOpen(Accessor),
    RightBracket,
    Semicolon,
    Comma,
    Dot,
    Colon,
    Question,
    Assign,
    AddAssign,
    SubtractAssign,
    MultiplyAssign,
    DivideAssign,
    ModuloAssign,
    BitOrAssign,
    BitAndAssign,
    BitXorAssign,
    LogicalOr,
    LogicalAnd,
    LogicalXor,
    Not,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    BitOr,
    BitAnd,
    BitXor,
    BitNot,
    ShiftLeft,
    ShiftRight,
    Add,
    Subtract,
    Multiply,
    Divide,
    IntegerDivide,
    Modulo,
    Increment,
    Decrement,
}

pub fn lex(source: &str) -> Result<Vec<Token>, Vec<Diagnostic>> {
    Lexer::new(source).run()
}

struct Lexer<'a> {
    source: &'a str,
    position: usize,
    line: u32,
    column: u32,
    tokens: Vec<Token>,
    errors: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            position: 0,
            line: 1,
            column: 1,
            tokens: Vec::with_capacity(source.len() / 4 + 1),
            errors: Vec::new(),
        }
    }

    fn run(mut self) -> Result<Vec<Token>, Vec<Diagnostic>> {
        if self.source.len() > u32::MAX as usize {
            return Err(vec![Diagnostic::new(
                "GML source is larger than 4 GiB",
                Span::default(),
            )]);
        }

        while self.position < self.source.len() {
            if self.skip_trivia() {
                continue;
            }
            let start = self.mark();
            let Some(character) = self.peek() else {
                break;
            };
            if character == '_' || character.is_alphabetic() {
                self.identifier(start);
            } else if character.is_ascii_digit()
                || (character == '.' && self.peek_second().is_some_and(|c| c.is_ascii_digit()))
                || (character == '$' && self.peek_second().is_some_and(|c| c.is_ascii_hexdigit()))
            {
                self.number(start);
            } else if character == '\'' || character == '"' {
                self.string(start, character);
            } else {
                self.symbol(start);
            }
        }

        let span = self.mark().finish(self.position);
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span,
        });
        if self.errors.is_empty() {
            Ok(self.tokens)
        } else {
            Err(self.errors)
        }
    }

    fn skip_trivia(&mut self) -> bool {
        let Some(character) = self.peek() else {
            return false;
        };
        if character.is_whitespace() {
            self.advance();
            return true;
        }
        if self.starts_with("//") {
            self.advance();
            self.advance();
            while self.peek().is_some_and(|c| c != '\n') {
                self.advance();
            }
            return true;
        }
        if self.starts_with("/*") {
            let start = self.mark();
            self.advance();
            self.advance();
            while self.position < self.source.len() && !self.starts_with("*/") {
                self.advance();
            }
            if self.starts_with("*/") {
                self.advance();
                self.advance();
            } else {
                self.errors.push(Diagnostic::new(
                    "unterminated block comment",
                    start.finish(self.position),
                ));
            }
            return true;
        }
        if character == '#' {
            while self.peek().is_some_and(|c| c != '\n') {
                self.advance();
            }
            return true;
        }
        false
    }

    fn identifier(&mut self, start: Mark) {
        self.advance();
        while self.peek().is_some_and(|c| c == '_' || c.is_alphanumeric()) {
            self.advance();
        }
        let span = start.finish(self.position);
        let text = &self.source[span.start as usize..span.end as usize];
        let kind = match text {
            "begin" => TokenKind::Begin,
            "end" => TokenKind::End,
            "if" => TokenKind::If,
            "then" => TokenKind::Then,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "do" => TokenKind::Do,
            "for" => TokenKind::For,
            "until" => TokenKind::Until,
            "repeat" => TokenKind::Repeat,
            "exit" => TokenKind::Exit,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "with" => TokenKind::With,
            "return" => TokenKind::Return,
            "switch" => TokenKind::Switch,
            "case" => TokenKind::Case,
            "default" => TokenKind::Default,
            "var" => TokenKind::Var,
            "globalvar" => TokenKind::GlobalVar,
            "enum" => TokenKind::Enum,
            "struct" => TokenKind::Struct,
            "and" => TokenKind::LogicalAnd,
            "or" => TokenKind::LogicalOr,
            "xor" => TokenKind::LogicalXor,
            "not" => TokenKind::Not,
            "div" => TokenKind::IntegerDivide,
            "mod" => TokenKind::Modulo,
            _ => TokenKind::Identifier,
        };
        self.tokens.push(Token { kind, span });
    }

    fn number(&mut self, start: Mark) {
        if self.peek() == Some('$') {
            self.advance();
            self.take_while(|c| c.is_ascii_hexdigit());
            self.push(TokenKind::Number, start);
            return;
        }
        if self.starts_with("0x") || self.starts_with("0X") {
            self.advance();
            self.advance();
            let digit_start = self.position;
            self.take_while(|c| c.is_ascii_hexdigit());
            if digit_start == self.position {
                self.errors.push(Diagnostic::new(
                    "hex literal requires at least one digit",
                    start.finish(self.position),
                ));
            }
            self.push(TokenKind::Number, start);
            return;
        }

        self.take_while(|c| c.is_ascii_digit());
        if self.peek() == Some('.') {
            self.advance();
            self.take_while(|c| c.is_ascii_digit());
        }
        if self.peek().is_some_and(|c| c == 'e' || c == 'E') {
            self.advance();
            if self.peek().is_some_and(|c| c == '+' || c == '-') {
                self.advance();
            }
            let digit_start = self.position;
            self.take_while(|c| c.is_ascii_digit());
            if digit_start == self.position {
                self.errors.push(Diagnostic::new(
                    "exponent requires at least one digit",
                    start.finish(self.position),
                ));
            }
        }
        self.push(TokenKind::Number, start);
    }

    fn string(&mut self, start: Mark, quote: char) {
        self.advance();
        let mut closed = false;
        while let Some(character) = self.peek() {
            self.advance();
            if character == quote {
                closed = true;
                break;
            }
            // The legacy GMS 1.4 lexer treats backslashes as ordinary string
            // characters. In particular, "\\" is a one-character string.
        }
        let span = start.finish(self.position);
        if !closed {
            self.errors
                .push(Diagnostic::new("unterminated string literal", span));
        }
        self.tokens.push(Token {
            kind: TokenKind::String,
            span,
        });
    }

    fn symbol(&mut self, start: Mark) {
        let pairs = [
            ("[b:", TokenKind::AccessorOpen(Accessor::Int8)),
            ("[x:", TokenKind::AccessorOpen(Accessor::Int16)),
            ("[i:", TokenKind::AccessorOpen(Accessor::Int32)),
            ("[B:", TokenKind::AccessorOpen(Accessor::UInt8)),
            ("[X:", TokenKind::AccessorOpen(Accessor::UInt16)),
            ("[I:", TokenKind::AccessorOpen(Accessor::UInt32)),
            ("[f:", TokenKind::AccessorOpen(Accessor::Float32)),
            ("[d:", TokenKind::AccessorOpen(Accessor::Float64)),
        ];
        for (text, kind) in pairs {
            if self.starts_with(text) {
                self.advance_bytes(text.len());
                self.push(kind, start);
                return;
            }
        }
        let pairs = [
            ("[?", TokenKind::AccessorOpen(Accessor::Map)),
            ("[#", TokenKind::AccessorOpen(Accessor::Grid)),
            ("[|", TokenKind::AccessorOpen(Accessor::List)),
            ("[@", TokenKind::AccessorOpen(Accessor::ArrayDirect)),
            ("<<", TokenKind::ShiftLeft),
            (">>", TokenKind::ShiftRight),
            ("<=", TokenKind::LessEqual),
            (">=", TokenKind::GreaterEqual),
            ("==", TokenKind::Equal),
            ("!=", TokenKind::NotEqual),
            ("<>", TokenKind::NotEqual),
            ("&&", TokenKind::LogicalAnd),
            ("||", TokenKind::LogicalOr),
            ("^^", TokenKind::LogicalXor),
            ("++", TokenKind::Increment),
            ("--", TokenKind::Decrement),
            ("+=", TokenKind::AddAssign),
            ("-=", TokenKind::SubtractAssign),
            ("*=", TokenKind::MultiplyAssign),
            ("/=", TokenKind::DivideAssign),
            ("%=", TokenKind::ModuloAssign),
            ("|=", TokenKind::BitOrAssign),
            ("&=", TokenKind::BitAndAssign),
            ("^=", TokenKind::BitXorAssign),
            (":=", TokenKind::Assign),
        ];
        for (text, kind) in pairs {
            if self.starts_with(text) {
                self.advance_bytes(text.len());
                self.push(kind, start);
                return;
            }
        }

        let Some(character) = self.advance() else {
            return;
        };
        let kind = match character {
            '{' => TokenKind::Begin,
            '}' => TokenKind::End,
            '(' => TokenKind::LeftParen,
            ')' => TokenKind::RightParen,
            '[' => TokenKind::AccessorOpen(Accessor::Array),
            ']' => TokenKind::RightBracket,
            ';' => TokenKind::Semicolon,
            ',' => TokenKind::Comma,
            '.' => TokenKind::Dot,
            ':' => TokenKind::Colon,
            '?' => TokenKind::Question,
            '=' => TokenKind::Assign,
            '!' => TokenKind::Not,
            '<' => TokenKind::Less,
            '>' => TokenKind::Greater,
            '|' => TokenKind::BitOr,
            '&' => TokenKind::BitAnd,
            '^' => TokenKind::BitXor,
            '~' => TokenKind::BitNot,
            '+' => TokenKind::Add,
            '-' => TokenKind::Subtract,
            '*' => TokenKind::Multiply,
            '/' => TokenKind::Divide,
            '%' => TokenKind::Modulo,
            _ => {
                self.errors.push(Diagnostic::new(
                    format!("unexpected character {character:?}"),
                    start.finish(self.position),
                ));
                return;
            }
        };
        self.push(kind, start);
    }

    fn push(&mut self, kind: TokenKind, start: Mark) {
        self.tokens.push(Token {
            kind,
            span: start.finish(self.position),
        });
    }

    fn take_while(&mut self, predicate: impl Fn(char) -> bool) {
        while self.peek().is_some_and(&predicate) {
            self.advance();
        }
    }

    fn starts_with(&self, text: &str) -> bool {
        self.source[self.position..].starts_with(text)
    }

    fn advance_bytes(&mut self, count: usize) {
        let end = self.position + count;
        while self.position < end {
            self.advance();
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.position..].chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        let mut chars = self.source[self.position..].chars();
        chars.next()?;
        chars.next()
    }

    fn advance(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.position += character.len_utf8();
        if character == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(character)
    }

    fn mark(&self) -> Mark {
        Mark {
            position: self.position,
            line: self.line,
            column: self.column,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Mark {
    position: usize,
    line: u32,
    column: u32,
}

impl Mark {
    fn finish(self, end: usize) -> Span {
        Span {
            start: self.position as u32,
            end: end as u32,
            line: self.line,
            column: self.column,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_legacy_operators_and_accessors() {
        let tokens = lex("a[? k] += $ff div 2 and b <> c; x[@ 0]++").unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|token| token.kind).collect();
        assert_eq!(
            kinds,
            [
                TokenKind::Identifier,
                TokenKind::AccessorOpen(Accessor::Map),
                TokenKind::Identifier,
                TokenKind::RightBracket,
                TokenKind::AddAssign,
                TokenKind::Number,
                TokenKind::IntegerDivide,
                TokenKind::Number,
                TokenKind::LogicalAnd,
                TokenKind::Identifier,
                TokenKind::NotEqual,
                TokenKind::Identifier,
                TokenKind::Semicolon,
                TokenKind::Identifier,
                TokenKind::AccessorOpen(Accessor::ArrayDirect),
                TokenKind::Number,
                TokenKind::RightBracket,
                TokenKind::Increment,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn tracks_unicode_source_positions() {
        let tokens = lex("// 注释\n变量 = 1").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Identifier);
        assert_eq!(tokens[0].span.line, 2);
        assert_eq!(tokens[0].span.column, 1);
        assert_eq!(
            &"// 注释\n变量 = 1"[tokens[0].span.start as usize..tokens[0].span.end as usize],
            "变量"
        );
    }

    #[test]
    fn reports_unterminated_input() {
        let error = lex("/* missing").unwrap_err();
        assert_eq!(error[0].span.line, 1);
        assert!(error[0].message.contains("unterminated"));
    }
}
