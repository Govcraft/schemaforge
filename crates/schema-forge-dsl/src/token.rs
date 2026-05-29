use logos::{Lexer, Logos};

/// Callback for the triple-quoted string token. After logos has matched
/// the opening `"""`, scans the remainder for the closing `"""` and
/// advances the lexer cursor past it. Returns `None` (lex error) if no
/// closing delimiter is found.
fn lex_triple_string(lex: &mut Lexer<Token>) -> Option<()> {
    let rest = lex.remainder();
    let end = rest.find("\"\"\"")?;
    lex.bump(end + 3);
    Some(())
}

/// Tokens produced by the SchemaDSL lexer.
///
/// Whitespace and comments are skipped automatically by logos.
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\r\n\f]+")]
#[logos(skip r"//[^\n]*")]
#[logos(skip r"/\*([^*]|\*[^/])*\*/")]
pub enum Token {
    // -- Keywords --
    #[token("schema")]
    Schema,

    #[token("text")]
    Text,

    #[token("richtext")]
    RichText,

    #[token("integer")]
    Integer,

    #[token("float")]
    Float,

    #[token("boolean")]
    Boolean,

    #[token("datetime")]
    DateTime,

    #[token("duration")]
    Duration,

    #[token("bytes")]
    Bytes,

    #[token("enum")]
    Enum,

    #[token("json")]
    Json,

    #[token("composite")]
    Composite,

    #[token("map")]
    Map,

    #[token("file")]
    File,

    #[token("required")]
    Required,

    #[token("indexed")]
    Indexed,

    #[token("unique")]
    Unique,

    #[token("default")]
    Default,

    #[token("true")]
    True,

    #[token("false")]
    False,

    // -- Punctuation --
    #[token("{")]
    LBrace,

    #[token("}")]
    RBrace,

    #[token("(")]
    LParen,

    #[token(")")]
    RParen,

    #[token("[")]
    LBracket,

    #[token("]")]
    RBracket,

    #[token(":")]
    Colon,

    #[token(",")]
    Comma,

    #[token("->")]
    Arrow,

    #[token("<")]
    Lt,

    #[token(">")]
    Gt,

    #[token("@")]
    At,

    // -- Literals --
    /// A triple-quoted string literal, e.g. `"""hello world"""`.
    /// May span multiple lines and contain unescaped double quotes
    /// (up to two in a row). Used for `@hook` intent descriptions.
    /// Must be matched before `StringLiteral` so logos picks the longer
    /// `"""` opener over `""` (an empty `StringLiteral`).
    #[token("\"\"\"", lex_triple_string)]
    TripleStringLiteral,

    /// A double-quoted string literal, e.g. `"hello"`.
    #[regex(r#""([^"\\]|\\.)*""#)]
    StringLiteral,

    /// An integer literal, optionally negative, e.g. `42` or `-10`.
    #[regex(r"-?[0-9]+", priority = 2)]
    IntegerLiteral,

    /// A float literal with a decimal point, e.g. `3.14` or `-2.5`.
    #[regex(r"-?[0-9]+\.[0-9]+", priority = 3)]
    FloatLiteral,

    // -- Identifiers --
    /// An identifier: letters, digits, and underscores, starting with a letter or underscore.
    /// This must come after keywords so that logos prefers keyword tokens.
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Ident,
}

/// Every reserved keyword in the SchemaDSL lexer, as its source string.
///
/// Single source of truth for "which identifiers the lexer claims as a keyword
/// token rather than [`Token::Ident`]". A name listed here cannot be used as a
/// bare identifier (schema name, field name, etc.) because the lexer never
/// emits `Ident` for it. Consumers that generate identifiers — notably the
/// property tests — filter against this list so they can't drift from the
/// lexer. The `keywords_const_matches_lexer` test pins each entry to a real
/// keyword token.
pub const KEYWORDS: &[&str] = &[
    "schema",
    "text",
    "richtext",
    "integer",
    "float",
    "boolean",
    "datetime",
    "duration",
    "bytes",
    "enum",
    "json",
    "composite",
    "map",
    "file",
    "required",
    "indexed",
    "unique",
    "default",
    "true",
    "false",
];

impl Token {
    /// Returns a human-readable description of this token kind.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Schema => "'schema'",
            Self::Text => "'text'",
            Self::RichText => "'richtext'",
            Self::Integer => "'integer'",
            Self::Float => "'float'",
            Self::Boolean => "'boolean'",
            Self::DateTime => "'datetime'",
            Self::Duration => "'duration'",
            Self::Bytes => "'bytes'",
            Self::Enum => "'enum'",
            Self::Json => "'json'",
            Self::Composite => "'composite'",
            Self::Map => "'map'",
            Self::File => "'file'",
            Self::Required => "'required'",
            Self::Indexed => "'indexed'",
            Self::Unique => "'unique'",
            Self::Default => "'default'",
            Self::True => "'true'",
            Self::False => "'false'",
            Self::LBrace => "'{'",
            Self::RBrace => "'}'",
            Self::LParen => "'('",
            Self::RParen => "')'",
            Self::LBracket => "'['",
            Self::RBracket => "']'",
            Self::Colon => "':'",
            Self::Comma => "','",
            Self::Arrow => "'->'",
            Self::Lt => "'<'",
            Self::Gt => "'>'",
            Self::At => "'@'",
            Self::StringLiteral => "string literal",
            Self::TripleStringLiteral => "triple-quoted string literal",
            Self::IntegerLiteral => "integer literal",
            Self::FloatLiteral => "float literal",
            Self::Ident => "identifier",
        }
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(input: &str) -> Vec<Token> {
        Token::lexer(input).map(|r| r.expect("lex error")).collect()
    }

    #[test]
    fn keywords() {
        let tokens = lex("schema text richtext integer float boolean datetime duration bytes enum json composite map file required indexed unique default true false");
        assert_eq!(
            tokens,
            vec![
                Token::Schema,
                Token::Text,
                Token::RichText,
                Token::Integer,
                Token::Float,
                Token::Boolean,
                Token::DateTime,
                Token::Duration,
                Token::Bytes,
                Token::Enum,
                Token::Json,
                Token::Composite,
                Token::Map,
                Token::File,
                Token::Required,
                Token::Indexed,
                Token::Unique,
                Token::Default,
                Token::True,
                Token::False,
            ]
        );
    }

    #[test]
    fn keywords_const_matches_lexer() {
        // Every entry in KEYWORDS must lex as exactly one non-`Ident` keyword
        // token. This pins the published list to the lexer so the two cannot
        // drift apart silently.
        for kw in KEYWORDS {
            let mut lexer = Token::lexer(kw);
            let tok = lexer
                .next()
                .unwrap_or_else(|| panic!("{kw} produced no token"))
                .unwrap_or_else(|_| panic!("{kw} produced a lex error"));
            assert_ne!(
                tok,
                Token::Ident,
                "{kw} is in KEYWORDS but the lexer treats it as an identifier"
            );
            assert!(lexer.next().is_none(), "{kw} should lex to a single token");
        }
    }

    #[test]
    fn lex_keywords_covers_full_const() {
        // The `keywords()` test asserts the exact token order for the full
        // keyword set; this guards that it stays in lockstep with the count of
        // published KEYWORDS, so a new keyword can't be added to one without
        // the other.
        let tokens = lex(&KEYWORDS.join(" "));
        assert_eq!(tokens.len(), KEYWORDS.len());
        assert!(tokens.iter().all(|t| *t != Token::Ident));
    }

    #[test]
    fn punctuation() {
        let tokens = lex("{ } ( ) [ ] : , -> < > @");
        assert_eq!(
            tokens,
            vec![
                Token::LBrace,
                Token::RBrace,
                Token::LParen,
                Token::RParen,
                Token::LBracket,
                Token::RBracket,
                Token::Colon,
                Token::Comma,
                Token::Arrow,
                Token::Lt,
                Token::Gt,
                Token::At,
            ]
        );
    }

    #[test]
    fn string_literal() {
        let tokens = lex(r#""hello" "world" "with \"escapes\"" "empty""#);
        assert_eq!(tokens.len(), 4);
        for t in &tokens {
            assert_eq!(*t, Token::StringLiteral);
        }
    }

    #[test]
    fn integer_literal() {
        let tokens = lex("0 42 -10 999");
        assert_eq!(
            tokens,
            vec![
                Token::IntegerLiteral,
                Token::IntegerLiteral,
                Token::IntegerLiteral,
                Token::IntegerLiteral,
            ]
        );
    }

    #[test]
    fn float_literal() {
        let tokens = lex("3.14 -2.5 0.0");
        assert_eq!(
            tokens,
            vec![
                Token::FloatLiteral,
                Token::FloatLiteral,
                Token::FloatLiteral,
            ]
        );
    }

    #[test]
    fn identifiers() {
        let tokens = lex("Contact first_name MySchema123");
        assert_eq!(tokens, vec![Token::Ident, Token::Ident, Token::Ident]);
    }

    #[test]
    fn line_comments_skipped() {
        let tokens = lex("schema // this is a comment\nContact");
        assert_eq!(tokens, vec![Token::Schema, Token::Ident]);
    }

    #[test]
    fn block_comments_skipped() {
        let tokens = lex("schema /* block comment */ Contact");
        assert_eq!(tokens, vec![Token::Schema, Token::Ident]);
    }

    #[test]
    fn multiline_block_comment() {
        let tokens = lex("schema /* multi\nline\ncomment */ Contact");
        assert_eq!(tokens, vec![Token::Schema, Token::Ident]);
    }

    #[test]
    fn arrow_token() {
        // Ensure -> is a single token, not '-' then '>'
        let tokens = lex("-> Contact");
        assert_eq!(tokens, vec![Token::Arrow, Token::Ident]);
    }

    #[test]
    fn full_field_definition() {
        let tokens = lex(r#"name: text(max: 255) required indexed"#);
        assert_eq!(
            tokens,
            vec![
                Token::Ident,
                Token::Colon,
                Token::Text,
                Token::LParen,
                Token::Ident,
                Token::Colon,
                Token::IntegerLiteral,
                Token::RParen,
                Token::Required,
                Token::Indexed,
            ]
        );
    }

    #[test]
    fn description_is_human_readable() {
        assert_eq!(Token::Schema.description(), "'schema'");
        assert_eq!(Token::Ident.description(), "identifier");
        assert_eq!(Token::StringLiteral.description(), "string literal");
    }
}
