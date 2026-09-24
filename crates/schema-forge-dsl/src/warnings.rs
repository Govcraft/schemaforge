//! Nonbreaking diagnostics for ambiguous authorization grants.

use crate::lexer::tokenize;
use crate::token::Token;
use crate::Span;

/// An explicit empty access grant, which permits every authenticated user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyAccessGrant {
    /// Annotation containing the grant (`access` or `field_access`).
    pub annotation: String,
    /// Direction with the explicit empty grant.
    pub direction: String,
    /// Location of the direction and list in the original source.
    pub span: Span,
}

impl std::fmt::Display for EmptyAccessGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}({}: []) grants access to every authenticated user; use a nonempty role list such as [\"platform_admin\"] to restrict access", self.annotation, self.direction)
    }
}

/// Report explicit empty read/write/delete grants without changing semantics.
/// Comments, string contents, omitted grants and cross_tenant_read are ignored.
/// Invalid token streams are left to the parser's error diagnostics.
pub fn empty_access_grants(source: &str) -> Vec<EmptyAccessGrant> {
    let Ok(tokens) = tokenize(source) else { return vec![] };
    let mut warnings = Vec::new();
    let mut annotation = None;
    for (index, token) in tokens.iter().enumerate() {
        if token.token == Token::At {
            annotation = tokens.get(index + 1).and_then(|name| {
                matches!(name.text.as_str(), "access" | "field_access").then_some(name.text.as_str())
            });
        }
        if token.token == Token::RParen {
            annotation = None;
        }
        let Some(annotation) = annotation else { continue };
        if !matches!(token.text.as_str(), "read" | "write" | "delete") {
            continue;
        }
        let Some(rest) = tokens.get(index + 1..index + 4) else { continue };
        if rest[0].token == Token::Colon && rest[1].token == Token::LBracket && rest[2].token == Token::RBracket {
            warnings.push(EmptyAccessGrant { annotation: annotation.into(), direction: token.text.clone(), span: Span::new(token.span.start, rest[2].span.end) });
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_only_explicit_empty_access_directions() {
        let source = r#"
            // @access(read: [])
            @access(read: ["staff"], write: [], cross_tenant_read: [])
            schema Setting {
                value: text @field_access(read: [], write: ["admin"])
                comment: text default("@access(delete: [])")
            }
        "#;
        let warnings = empty_access_grants(source);
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].direction, "write");
        assert_eq!(warnings[1].annotation, "field_access");
        assert_eq!(&source[warnings[0].span.start..warnings[0].span.end], "write: []");
    }
}
