//! Hand-written lexer for the CEL expression grammar (#107).
//!
//! Pure `std`: no regex, no generated tables. The scanner walks the source byte
//! by byte while tracking a [`Position`] (byte offset + 1-based line/column), so
//! every token — and every lexical error — can point at its place in the source.
//!
//! String/bytes decoding follows the cel-spec escaping rules exactly (the
//! `string_literals` / `bytes_literals` sections of the conformance `parse`
//! corpus are the authority). The decoders are small pure functions so they can
//! be unit-tested in isolation.

use crate::error::{ParseError, Position};

/// A lexical token kind.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Tok {
    /// A signed integer literal (`int`).
    Int(i64),
    /// The magnitude `2^63` (`9223372036854775808`), which does not fit `i64`.
    ///
    /// CEL has no negative integer literal token, so `i64::MIN` is written as
    /// unary minus applied to this magnitude. It is legal *only* as the immediate
    /// operand of a unary `-` (the parser folds `-` + this into `Int(i64::MIN)`);
    /// anywhere else it is an out-of-range error.
    IntMinMagnitude,
    /// An unsigned integer literal (`uint`, written with a `u`/`U` suffix).
    Uint(u64),
    /// A floating-point literal (`double`).
    Double(f64),
    /// A decoded string literal.
    Str(String),
    /// A decoded bytes literal.
    Bytes(Vec<u8>),
    /// A non-reserved identifier.
    Ident(String),
    /// The `true` literal keyword.
    True,
    /// The `false` literal keyword.
    False,
    /// The `null` literal keyword.
    Null,
    /// The `in` relational keyword.
    In,
    /// A reserved word (illegal as a bare identifier, but legal as a field
    /// selector, method name, or struct field name). Carries its text.
    Reserved(String),
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBrack,
    /// `]`
    RBrack,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `.`
    Dot,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `?`
    Question,
    /// `||`
    Or,
    /// `&&`
    And,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `!`
    Not,
    /// End of input.
    Eof,
}

/// A token together with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// The token kind.
    pub tok: Tok,
    /// The position of the token's first byte.
    pub pos: Position,
}

/// Reserved words that may not be used as bare identifiers (but are allowed as
/// selectors, method names, and struct field names).
const RESERVED: &[&str] = &[
    "as",
    "break",
    "const",
    "continue",
    "else",
    "for",
    "function",
    "if",
    "import",
    "let",
    "loop",
    "package",
    "namespace",
    "return",
    "var",
    "void",
    "while",
];

/// Tokenize `src` into a flat token stream terminated by [`Tok::Eof`].
///
/// # Errors
/// Returns a positioned [`ParseError`] on any unrecognized character, malformed
/// number, or malformed string/bytes literal.
pub fn lex(src: &str) -> Result<Vec<Token>, ParseError> {
    Lexer::new(src).run()
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    idx: usize,
    line: usize,
    column: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src: src.as_bytes(),
            text: src,
            idx: 0,
            line: 1,
            column: 1,
        }
    }

    fn pos(&self) -> Position {
        Position {
            offset: self.idx,
            line: self.line,
            column: self.column,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.idx).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.src.get(self.idx + ahead).copied()
    }

    /// Advance one byte, maintaining line/column. Column counts chars: we only
    /// increment column on a UTF-8 lead byte (a byte that is not a continuation
    /// byte `10xxxxxx`).
    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.idx += 1;
        if b == b'\n' {
            self.line += 1;
            self.column = 1;
        } else if b & 0xC0 != 0x80 {
            self.column += 1;
        }
        Some(b)
    }

    fn run(mut self) -> Result<Vec<Token>, ParseError> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia();
            let start = self.pos();
            let Some(b) = self.peek() else {
                out.push(Token {
                    tok: Tok::Eof,
                    pos: start,
                });
                return Ok(out);
            };
            let tok = self.next_token(b, start)?;
            out.push(Token { tok, pos: start });
        }
    }

    /// Skip whitespace and `//` line comments.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0C) => {
                    self.bump();
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    // Comment runs to the next line feed. A lone carriage return
                    // does not terminate it (cel-spec `comments` section).
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    fn next_token(&mut self, b: u8, start: Position) -> Result<Tok, ParseError> {
        // String/bytes literal, possibly with an r/R/b/B prefix.
        if let Some(prefix_len) = self.string_prefix() {
            return self.scan_string(prefix_len, start);
        }
        if b == b'"' || b == b'\'' {
            return self.scan_string(0, start);
        }
        if b.is_ascii_digit() || (b == b'.' && self.dot_starts_number()) {
            return self.scan_number(start);
        }
        if b == b'_' || b.is_ascii_alphabetic() {
            return Ok(self.scan_word());
        }
        self.scan_operator(b, start)
    }

    /// If the upcoming bytes are a valid string/bytes prefix immediately followed
    /// by a quote, return the prefix length. Prefixes are any combination of one
    /// `r`/`R` and one `b`/`B` (case-insensitive, order-free).
    fn string_prefix(&self) -> Option<usize> {
        let mut i = 0;
        let mut seen_r = false;
        let mut seen_b = false;
        while let Some(c) = self.src.get(self.idx + i).copied() {
            match c {
                b'r' | b'R' if !seen_r => seen_r = true,
                b'b' | b'B' if !seen_b => seen_b = true,
                _ => break,
            }
            i += 1;
        }
        if i == 0 {
            return None;
        }
        match self.src.get(self.idx + i).copied() {
            Some(b'"' | b'\'') => Some(i),
            _ => None,
        }
    }

    /// Whether a `.` at the cursor begins a floating-point literal. True only
    /// when followed by a digit and not immediately preceded by a character that
    /// would make it a member selection (ident char, `)`, or `]`). This is the
    /// one context-sensitive rule in the lexer, documented in the module header.
    fn dot_starts_number(&self) -> bool {
        if !matches!(self.peek_at(1), Some(c) if c.is_ascii_digit()) {
            return false;
        }
        let prev = self
            .idx
            .checked_sub(1)
            .and_then(|i| self.src.get(i).copied());
        !matches!(prev, Some(p) if p == b'_' || p == b')' || p == b']' || p.is_ascii_alphanumeric())
    }

    fn scan_word(&mut self) -> Tok {
        let start = self.idx;
        while let Some(c) = self.peek() {
            if c == b'_' || c.is_ascii_alphanumeric() {
                self.bump();
            } else {
                break;
            }
        }
        let word = &self.text[start..self.idx];
        match word {
            "true" => Tok::True,
            "false" => Tok::False,
            "null" => Tok::Null,
            "in" => Tok::In,
            _ if RESERVED.contains(&word) => Tok::Reserved(word.to_string()),
            _ => Tok::Ident(word.to_string()),
        }
    }

    fn scan_operator(&mut self, b: u8, start: Position) -> Result<Tok, ParseError> {
        self.bump();
        let tok = match b {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'[' => Tok::LBrack,
            b']' => Tok::RBrack,
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b'.' => Tok::Dot,
            b',' => Tok::Comma,
            b':' => Tok::Colon,
            b'?' => Tok::Question,
            b'+' => Tok::Plus,
            b'-' => Tok::Minus,
            b'*' => Tok::Star,
            b'/' => Tok::Slash,
            b'%' => Tok::Percent,
            b'=' if self.peek() == Some(b'=') => {
                self.bump();
                Tok::Eq
            }
            b'!' if self.peek() == Some(b'=') => {
                self.bump();
                Tok::Ne
            }
            b'!' => Tok::Not,
            b'<' if self.peek() == Some(b'=') => {
                self.bump();
                Tok::Le
            }
            b'<' => Tok::Lt,
            b'>' if self.peek() == Some(b'=') => {
                self.bump();
                Tok::Ge
            }
            b'>' => Tok::Gt,
            b'&' if self.peek() == Some(b'&') => {
                self.bump();
                Tok::And
            }
            b'|' if self.peek() == Some(b'|') => {
                self.bump();
                Tok::Or
            }
            other => {
                return Err(ParseError::with_position(
                    format!("unexpected character '{}'", other as char),
                    start,
                ));
            }
        };
        Ok(tok)
    }

    fn scan_number(&mut self, start: Position) -> Result<Tok, ParseError> {
        let begin = self.idx;
        // Hex integer.
        if self.peek() == Some(b'0') && matches!(self.peek_at(1), Some(b'x' | b'X')) {
            self.bump();
            self.bump();
            let digits_start = self.idx;
            while matches!(self.peek(), Some(c) if c.is_ascii_hexdigit()) {
                self.bump();
            }
            if self.idx == digits_start {
                return Err(ParseError::with_position("malformed hex literal", start));
            }
            let hex = &self.text[digits_start..self.idx];
            return self.finish_int(hex, 16, start);
        }

        let mut is_double = false;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.bump();
        }
        if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(c) if c.is_ascii_digit()) {
            is_double = true;
            self.bump();
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        } else if self.peek() == Some(b'.') && begin == self.idx {
            // Leading-dot double like `.5`.
            is_double = true;
            self.bump();
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_double = true;
            self.bump();
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.bump();
            }
            let exp_start = self.idx;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
            if self.idx == exp_start {
                return Err(ParseError::with_position("malformed exponent", start));
            }
        }

        if is_double {
            let text = &self.text[begin..self.idx];
            return text
                .parse::<f64>()
                .map(Tok::Double)
                .map_err(|_| ParseError::with_position("malformed double literal", start));
        }

        let digits = &self.text[begin..self.idx];
        self.finish_int(digits, 10, start)
    }

    /// Parse an integer's digits (already scanned) honoring an optional `u`/`U`
    /// suffix. `radix` is 10 or 16.
    fn finish_int(&mut self, digits: &str, radix: u32, start: Position) -> Result<Tok, ParseError> {
        if matches!(self.peek(), Some(b'u' | b'U')) {
            self.bump();
            return u64::from_str_radix(digits, radix)
                .map(Tok::Uint)
                .map_err(|_| ParseError::with_position("uint literal out of range", start));
        }
        if let Ok(i) = i64::from_str_radix(digits, radix) {
            return Ok(Tok::Int(i));
        }
        // `2^63` overflows `i64` but is legal as the operand of unary minus
        // (yielding `i64::MIN`); the parser folds it. Any larger magnitude is a
        // genuine out-of-range error.
        if u64::from_str_radix(digits, radix) == Ok(1u64 << 63) {
            return Ok(Tok::IntMinMagnitude);
        }
        Err(ParseError::with_position("int literal out of range", start))
    }

    /// Scan a string or bytes literal beginning at the cursor. `prefix_len` bytes
    /// of `r`/`R`/`b`/`B` prefix precede the opening quote.
    fn scan_string(&mut self, prefix_len: usize, start: Position) -> Result<Tok, ParseError> {
        let mut raw = false;
        let mut bytes = false;
        for _ in 0..prefix_len {
            match self.bump() {
                Some(b'r' | b'R') => raw = true,
                Some(b'b' | b'B') => bytes = true,
                _ => unreachable!("prefix already validated"),
            }
        }

        let quote = self.peek().expect("quote follows validated prefix");
        let triple = self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote);
        let quote_len = if triple { 3 } else { 1 };
        for _ in 0..quote_len {
            self.bump();
        }

        let body_start = self.idx;
        let body_end = self.scan_string_body(quote, triple, start)?;
        let body = &self.text[body_start..body_end];
        for _ in 0..quote_len {
            self.bump();
        }

        if raw {
            return Ok(if bytes {
                Tok::Bytes(body.as_bytes().to_vec())
            } else {
                Tok::Str(body.to_string())
            });
        }
        if bytes {
            decode_bytes(body, start).map(Tok::Bytes)
        } else {
            decode_string(body, start).map(Tok::Str)
        }
    }

    /// Advance the cursor to (but not over) the closing quote, returning the byte
    /// offset where the body ends. Honors raw vs escaped termination and triple
    /// vs single quoting.
    fn scan_string_body(
        &mut self,
        quote: u8,
        triple: bool,
        start: Position,
    ) -> Result<usize, ParseError> {
        loop {
            let Some(c) = self.peek() else {
                return Err(ParseError::with_position(
                    "unterminated string literal",
                    start,
                ));
            };
            if c == quote {
                if triple {
                    if self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote) {
                        return Ok(self.idx);
                    }
                } else {
                    return Ok(self.idx);
                }
            }
            if !triple && (c == b'\n' || c == b'\r') {
                return Err(ParseError::with_position(
                    "unterminated string literal (newline in single-quoted string)",
                    start,
                ));
            }
            // A backslash escapes the next byte for termination purposes (so an
            // escaped quote does not end the string). Raw strings keep the
            // backslash but still let it shield the following byte from being
            // read as the terminator.
            if c == b'\\' {
                self.bump();
                if self.peek().is_none() {
                    return Err(ParseError::with_position(
                        "unterminated string literal",
                        start,
                    ));
                }
            }
            self.bump();
        }
    }
}

/// Decode the body of a (non-raw) string literal into a `String`.
///
/// Escapes `\xHH`, `\OOO` octal, and the simple escapes map to code points (a
/// single `char`); `\uHHHH` / `\UHHHHHHHH` map to Unicode scalar values.
///
/// # Errors
/// Returns a positioned [`ParseError`] for any malformed or unknown escape.
pub fn decode_string(body: &str, start: Position) -> Result<String, ParseError> {
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            // Copy the full UTF-8 char.
            let ch = body[i..].chars().next().expect("valid utf-8 boundary");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let esc = decode_escape(bytes, &mut i, false, start)?;
        match esc {
            Escape::Char(ch) => out.push(ch),
            Escape::Byte(b) => out.push(char::from(b)),
        }
    }
    Ok(out)
}

/// Decode the body of a (non-raw) bytes literal into a `Vec<u8>`.
///
/// `\xHH` and `\OOO` produce raw bytes; `\u`/`\U` are rejected in bytes literals.
///
/// # Errors
/// Returns a positioned [`ParseError`] for any malformed or unknown escape, or a
/// `\u`/`\U` escape (invalid in bytes).
pub fn decode_bytes(body: &str, start: Position) -> Result<Vec<u8>, ParseError> {
    let mut out = Vec::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let esc = decode_escape(bytes, &mut i, true, start)?;
        match esc {
            Escape::Byte(b) => out.push(b),
            Escape::Char(ch) => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    Ok(out)
}

/// The decoded result of a single escape sequence.
enum Escape {
    /// A Unicode scalar value (from `\u`/`\U`, or a simple escape).
    Char(char),
    /// A raw byte value (from `\xHH` or `\OOO` octal).
    Byte(u8),
}

/// Decode one escape sequence starting at `bytes[*i] == b'\\'`, advancing `*i`
/// past it. `is_bytes` selects bytes-literal rules (rejects `\u`/`\U`).
fn decode_escape(
    bytes: &[u8],
    i: &mut usize,
    is_bytes: bool,
    start: Position,
) -> Result<Escape, ParseError> {
    *i += 1; // consume backslash
    let Some(&c) = bytes.get(*i) else {
        return Err(ParseError::with_position(
            "trailing backslash in literal",
            start,
        ));
    };
    *i += 1;
    let simple = match c {
        b'\\' => Some('\\'),
        b'"' => Some('"'),
        b'\'' => Some('\''),
        b'`' => Some('`'),
        b'?' => Some('?'),
        b'a' => Some('\u{07}'),
        b'b' => Some('\u{08}'),
        b'f' => Some('\u{0C}'),
        b'n' => Some('\n'),
        b'r' => Some('\r'),
        b't' => Some('\t'),
        b'v' => Some('\u{0B}'),
        _ => None,
    };
    if let Some(ch) = simple {
        return Ok(Escape::Char(ch));
    }
    match c {
        b'0'..=b'7' => decode_octal(bytes, i, c, start),
        b'x' | b'X' => decode_hex(bytes, i, 2, start).map(|v| Escape::Byte(v as u8)),
        b'u' if !is_bytes => decode_unicode(bytes, i, 4, start),
        b'U' if !is_bytes => decode_unicode(bytes, i, 8, start),
        b'u' | b'U' => Err(ParseError::with_position(
            "unicode escape is not valid in a bytes literal",
            start,
        )),
        other => Err(ParseError::with_position(
            format!("unknown escape sequence '\\{}'", other as char),
            start,
        )),
    }
}

/// Decode an octal escape `\OOO`. `first` is the first octal digit (already
/// consumed); two more are required.
fn decode_octal(
    bytes: &[u8],
    i: &mut usize,
    first: u8,
    start: Position,
) -> Result<Escape, ParseError> {
    let mut value = (first - b'0') as u32;
    for _ in 0..2 {
        match bytes.get(*i) {
            Some(d @ b'0'..=b'7') => {
                value = value * 8 + u32::from(d - b'0');
                *i += 1;
            }
            _ => {
                return Err(ParseError::with_position(
                    "octal escape requires three octal digits",
                    start,
                ))
            }
        }
    }
    if value > 0xFF {
        return Err(ParseError::with_position(
            "octal escape out of range",
            start,
        ));
    }
    Ok(Escape::Byte(value as u8))
}

/// Decode `count` hex digits into a value.
fn decode_hex(
    bytes: &[u8],
    i: &mut usize,
    count: usize,
    start: Position,
) -> Result<u32, ParseError> {
    let mut value: u32 = 0;
    for _ in 0..count {
        match bytes.get(*i) {
            Some(d) if d.is_ascii_hexdigit() => {
                let digit = (*d as char).to_digit(16).expect("ascii hex digit");
                value = value * 16 + digit;
                *i += 1;
            }
            _ => {
                return Err(ParseError::with_position(
                    "hex escape requires the expected number of hex digits",
                    start,
                ))
            }
        }
    }
    Ok(value)
}

/// Decode a `\u` (count = 4) or `\U` (count = 8) Unicode escape into a `char`.
fn decode_unicode(
    bytes: &[u8],
    i: &mut usize,
    count: usize,
    start: Position,
) -> Result<Escape, ParseError> {
    let value = decode_hex(bytes, i, count, start)?;
    match char::from_u32(value) {
        Some(ch) => Ok(Escape::Char(ch)),
        None => Err(ParseError::with_position(
            "unicode escape is not a valid code point",
            start,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        lex(src)
            .unwrap()
            .into_iter()
            .map(|t| t.tok)
            .filter(|t| *t != Tok::Eof)
            .collect()
    }

    #[test]
    fn integers_decimal_hex_uint() {
        assert_eq!(toks("42"), vec![Tok::Int(42)]);
        assert_eq!(toks("0xFF"), vec![Tok::Int(255)]);
        assert_eq!(toks("0X10"), vec![Tok::Int(16)]);
        assert_eq!(toks("7u"), vec![Tok::Uint(7)]);
        assert_eq!(toks("0xffU"), vec![Tok::Uint(255)]);
    }

    #[test]
    fn doubles() {
        assert_eq!(toks("1.0"), vec![Tok::Double(1.0)]);
        assert_eq!(toks(".5"), vec![Tok::Double(0.5)]);
        assert_eq!(toks("1e3"), vec![Tok::Double(1000.0)]);
        assert_eq!(toks("1.5e-3"), vec![Tok::Double(0.0015)]);
        assert_eq!(toks("2E+2"), vec![Tok::Double(200.0)]);
    }

    #[test]
    fn dot_is_selection_not_number_after_ident() {
        // `a.b` must lex as ident, dot, ident — not a number.
        assert_eq!(
            toks("a.b"),
            vec![Tok::Ident("a".into()), Tok::Dot, Tok::Ident("b".into())]
        );
    }

    #[test]
    fn keywords_and_reserved() {
        assert_eq!(
            toks("true false null"),
            vec![Tok::True, Tok::False, Tok::Null]
        );
        assert_eq!(toks("in"), vec![Tok::In]);
        assert_eq!(toks("break"), vec![Tok::Reserved("break".into())]);
        assert_eq!(toks("foo"), vec![Tok::Ident("foo".into())]);
    }

    #[test]
    fn operators_maximal_munch() {
        assert_eq!(
            toks("== != <= >= < > && || ! + - * / %"),
            vec![
                Tok::Eq,
                Tok::Ne,
                Tok::Le,
                Tok::Ge,
                Tok::Lt,
                Tok::Gt,
                Tok::And,
                Tok::Or,
                Tok::Not,
                Tok::Plus,
                Tok::Minus,
                Tok::Star,
                Tok::Slash,
                Tok::Percent,
            ]
        );
    }

    #[test]
    fn lone_ampersand_errors_with_position() {
        let err = lex("a & b").unwrap_err();
        assert!(err.position().is_some());
    }

    #[test]
    fn string_quote_forms() {
        assert_eq!(toks("'hello'"), vec![Tok::Str("hello".into())]);
        assert_eq!(toks("\"hello\""), vec![Tok::Str("hello".into())]);
        assert_eq!(toks("'''hello'''"), vec![Tok::Str("hello".into())]);
        assert_eq!(toks("\"\"\"hello\"\"\""), vec![Tok::Str("hello".into())]);
    }

    #[test]
    fn string_escapes_punctuation_and_control() {
        assert_eq!(
            toks(r#"' \\ \? \" \' \` '"#),
            vec![Tok::Str(" \\ ? \" ' ` ".into())]
        );
        assert_eq!(
            toks(r"' \a \b \f \t \v \n \r '"),
            vec![Tok::Str(" \u{07} \u{08} \u{0C} \t \u{0B} \n \r ".into())]
        );
    }

    #[test]
    fn string_octal_hex_unicode_escapes() {
        assert_eq!(
            toks(r"' \000 \012 \177 '"),
            vec![Tok::Str(" \u{00} \n \u{7F} ".into())]
        );
        assert_eq!(
            toks(r"' \x00 \x7F '"),
            vec![Tok::Str(" \u{00} \u{7F} ".into())]
        );
        assert_eq!(
            toks(r"' Ā ￻ '"),
            vec![Tok::Str(" \u{0100} \u{FFFB} ".into())]
        );
        assert_eq!(
            toks(r"' \U00010000 \U0001F62C '"),
            vec![Tok::Str(" \u{10000} \u{1F62C} ".into())]
        );
        // Unassigned code point is still a valid scalar value.
        assert_eq!(
            toks(r"' \U00088888 '"),
            vec![Tok::Str(" \u{88888} ".into())]
        );
    }

    #[test]
    fn raw_strings_do_not_process_escapes() {
        assert_eq!(toks(r"r'\n\x00'"), vec![Tok::Str(r"\n\x00".into())]);
        assert_eq!(toks(r"R'\t'"), vec![Tok::Str(r"\t".into())]);
    }

    #[test]
    fn bytes_literals_and_prefixes() {
        assert_eq!(toks("b'hello'"), vec![Tok::Bytes(b"hello".to_vec())]);
        assert_eq!(toks(r"b'\x00\xFF'"), vec![Tok::Bytes(vec![0x00, 0xFF])]);
        assert_eq!(toks(r"br'\x00'"), vec![Tok::Bytes(br"\x00".to_vec())]);
        assert_eq!(toks(r"rb'\n'"), vec![Tok::Bytes(br"\n".to_vec())]);
        assert_eq!(toks(r"bR'\t'"), vec![Tok::Bytes(br"\t".to_vec())]);
    }

    #[test]
    fn unicode_escape_rejected_in_bytes() {
        // A plain byte is fine; \u and \U escapes are not valid in bytes.
        assert_eq!(toks("b'A'"), vec![Tok::Bytes(b"A".to_vec())]);
        let lower_u = r"b' \u0041 '";
        let upper_u = r"b'\U00000041'";
        assert!(lex(lower_u).unwrap_err().position().is_some());
        assert!(lex(upper_u).unwrap_err().position().is_some());
    }

    #[test]
    fn unterminated_string_errors() {
        assert!(lex("'oops").unwrap_err().position().is_some());
        assert!(lex("'no\nnewline'").unwrap_err().position().is_some());
    }

    #[test]
    fn bad_escape_errors() {
        assert!(lex(r"'\q'").unwrap_err().position().is_some());
        assert!(lex(r"'\x0'").unwrap_err().position().is_some());
        assert!(lex(r"'\09'").unwrap_err().position().is_some());
    }

    #[test]
    fn comments_and_whitespace() {
        assert_eq!(
            toks("1 // comment\n+ 2"),
            vec![Tok::Int(1), Tok::Plus, Tok::Int(2)]
        );
        // Form feed and carriage return are whitespace.
        assert_eq!(
            toks("1\u{0C}+\r2"),
            vec![Tok::Int(1), Tok::Plus, Tok::Int(2)]
        );
    }

    #[test]
    fn position_tracks_line_and_column() {
        let tokens = lex("a\n  b").unwrap();
        assert_eq!(tokens[1].pos.line, 2);
        assert_eq!(tokens[1].pos.column, 3);
    }
}
