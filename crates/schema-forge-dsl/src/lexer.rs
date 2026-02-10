use logos::Logos;

use crate::error::{DslError, Span};
use crate::token::Token;

/// A token paired with its source span.
#[derive(Debug, Clone)]
pub struct SpannedToken {
    pub token: Token,
    pub span: Span,
    pub text: String,
}

/// Tokenizes DSL source text into a sequence of spanned tokens.
///
/// Invalid tokens are collected as `DslError::InvalidToken` errors.
/// If any invalid tokens are found, the entire result is an error.
///
/// # Errors
///
/// Returns a list of `DslError::InvalidToken` for any bytes the lexer
/// cannot match to a valid token rule.
pub fn tokenize(source: &str) -> Result<Vec<SpannedToken>, Vec<DslError>> {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();

    let lexer = Token::lexer(source);
    for (result, range) in lexer.spanned() {
        let span = Span::new(range.start, range.end);
        match result {
            Ok(token) => {
                tokens.push(SpannedToken {
                    token,
                    span,
                    text: source[range].to_string(),
                });
            }
            Err(()) => {
                errors.push(DslError::InvalidToken { span });
            }
        }
    }

    if errors.is_empty() {
        Ok(tokens)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_simple_schema() {
        let tokens = tokenize("schema Contact { }").unwrap();
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].token, Token::Schema);
        assert_eq!(tokens[0].text, "schema");
        assert_eq!(tokens[1].token, Token::Ident);
        assert_eq!(tokens[1].text, "Contact");
        assert_eq!(tokens[2].token, Token::LBrace);
        assert_eq!(tokens[3].token, Token::RBrace);
    }

    #[test]
    fn tokenize_preserves_spans() {
        let tokens = tokenize("schema Contact").unwrap();
        assert_eq!(tokens[0].span, Span::new(0, 6));
        assert_eq!(tokens[1].span, Span::new(7, 14));
    }

    #[test]
    fn tokenize_invalid_character() {
        let result = tokenize("schema # Contact");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], DslError::InvalidToken { .. }));
    }

    #[test]
    fn tokenize_empty_input() {
        let tokens = tokenize("").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_whitespace_only() {
        let tokens = tokenize("   \n\t  ").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_comments_only() {
        let tokens = tokenize("// just a comment\n/* block */").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_string_with_escapes() {
        let tokens = tokenize(r#""hello \"world\"""#).unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].token, Token::StringLiteral);
    }
}
