use logos::Logos;

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

    #[token("enum")]
    Enum,

    #[token("json")]
    Json,

    #[token("composite")]
    Composite,

    #[token("required")]
    Required,

    #[token("indexed")]
    Indexed,

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

    #[token("@")]
    At,

    // -- Literals --
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
            Self::Enum => "'enum'",
            Self::Json => "'json'",
            Self::Composite => "'composite'",
            Self::Required => "'required'",
            Self::Indexed => "'indexed'",
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
            Self::At => "'@'",
            Self::StringLiteral => "string literal",
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
        let tokens = lex("schema text richtext integer float boolean datetime enum json composite required indexed default true false");
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
                Token::Enum,
                Token::Json,
                Token::Composite,
                Token::Required,
                Token::Indexed,
                Token::Default,
                Token::True,
                Token::False,
            ]
        );
    }

    #[test]
    fn punctuation() {
        let tokens = lex("{ } ( ) [ ] : , -> @");
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
