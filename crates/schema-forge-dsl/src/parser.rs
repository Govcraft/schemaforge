use std::collections::HashSet;

use schema_forge_core::types::{
    Annotation, Cardinality, DefaultValue, EnumVariants, FieldDefinition, FieldModifier, FieldName,
    FieldType, FloatConstraints, IntegerConstraints, SchemaDefinition, SchemaId, SchemaName,
    SchemaVersion, TextConstraints,
};

use crate::error::{DslError, Span};
use crate::lexer::SpannedToken;
use crate::token::Token;

/// Recursive descent parser for the SchemaDSL grammar.
///
/// Consumes a flat list of spanned tokens produced by the lexer
/// and produces a list of `SchemaDefinition` values.
struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<SpannedToken>) -> Self {
        Self { tokens, pos: 0 }
    }

    // -- Cursor helpers --

    fn peek(&self) -> Option<&SpannedToken> {
        self.tokens.get(self.pos)
    }

    fn peek_token(&self) -> Option<&Token> {
        self.peek().map(|st| &st.token)
    }

    fn advance(&mut self) -> Option<SpannedToken> {
        if self.pos < self.tokens.len() {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn expect(&mut self, expected: &Token) -> Result<SpannedToken, DslError> {
        match self.advance() {
            Some(st) if st.token == *expected => Ok(st),
            Some(st) => Err(DslError::UnexpectedToken {
                expected: expected.description().to_string(),
                found: format!("{} ('{}')", st.token.description(), st.text),
                span: st.span,
            }),
            None => Err(DslError::UnexpectedEndOfInput {
                expected: expected.description().to_string(),
            }),
        }
    }

    fn current_span(&self) -> Span {
        self.peek()
            .map(|st| st.span.clone())
            .unwrap_or_else(|| {
                // Point to end of last token, or 0..0 if empty
                self.tokens
                    .last()
                    .map(|st| Span::new(st.span.end, st.span.end))
                    .unwrap_or(Span::new(0, 0))
            })
    }

    // -- Grammar productions --

    /// file = schema_def*
    fn parse_file(&mut self) -> Result<Vec<SchemaDefinition>, Vec<DslError>> {
        let mut schemas = Vec::new();
        let mut errors = Vec::new();

        while self.peek().is_some() {
            match self.parse_schema() {
                Ok(schema) => schemas.push(schema),
                Err(e) => {
                    errors.push(e);
                    self.recover_to_next_schema();
                }
            }
        }

        if errors.is_empty() {
            Ok(schemas)
        } else {
            Err(errors)
        }
    }

    /// Skip tokens until we find the next `schema` keyword or `@` annotation at top level.
    fn recover_to_next_schema(&mut self) {
        let mut brace_depth: i32 = 0;
        while let Some(st) = self.peek() {
            match st.token {
                Token::LBrace => {
                    brace_depth += 1;
                    self.advance();
                }
                Token::RBrace => {
                    brace_depth -= 1;
                    self.advance();
                    if brace_depth <= 0 {
                        return;
                    }
                }
                Token::Schema | Token::At if brace_depth == 0 => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// schema_def = annotation* "schema" IDENT "{" field_def* "}"
    fn parse_schema(&mut self) -> Result<SchemaDefinition, DslError> {
        let schema_start = self.current_span().start;
        let annotations = self.parse_annotations()?;

        self.expect(&Token::Schema)?;

        let name_tok = self.expect_ident("schema name")?;
        let schema_name = SchemaName::new(&name_tok.text).map_err(|_| {
            DslError::InvalidSchemaName {
                name: name_tok.text.clone(),
                span: name_tok.span.clone(),
            }
        })?;

        self.expect(&Token::LBrace)?;

        let fields = self.parse_fields()?;

        let rbrace = self.expect(&Token::RBrace)?;
        let schema_span = Span::new(schema_start, rbrace.span.end);

        if fields.is_empty() {
            return Err(DslError::EmptySchema {
                name: schema_name.as_str().to_string(),
                span: schema_span,
            });
        }

        // Validate no duplicate field names
        let mut seen_names = HashSet::new();
        for field in &fields {
            if !seen_names.insert(field.name.as_str().to_string()) {
                return Err(DslError::DuplicateFieldName {
                    name: field.name.as_str().to_string(),
                    span: schema_span,
                });
            }
        }

        // Validate no duplicate annotation kinds
        let mut seen_kinds = HashSet::new();
        for ann in &annotations {
            if !seen_kinds.insert(ann.kind().to_string()) {
                return Err(DslError::DuplicateAnnotation {
                    kind: ann.kind().to_string(),
                    span: schema_span,
                });
            }
        }

        SchemaDefinition::new(SchemaId::new(), schema_name, fields, annotations).map_err(|e| {
            DslError::CoreSchemaError {
                source: e,
                span: schema_span,
            }
        })
    }

    /// annotation* (zero or more leading annotations)
    fn parse_annotations(&mut self) -> Result<Vec<Annotation>, DslError> {
        let mut annotations = Vec::new();
        while self.peek_token() == Some(&Token::At) {
            annotations.push(self.parse_annotation()?);
        }
        Ok(annotations)
    }

    /// annotation = "@" IDENT "(" literal ")"
    fn parse_annotation(&mut self) -> Result<Annotation, DslError> {
        self.expect(&Token::At)?;
        let name_tok = self.expect_ident("annotation name")?;
        self.expect(&Token::LParen)?;

        let annotation = match name_tok.text.as_str() {
            "version" => {
                let value_tok = self.expect_integer_literal()?;
                let version_num =
                    parse_i64(&value_tok.text, &value_tok.span)? as u32;
                let version = SchemaVersion::new(version_num).map_err(|e| {
                    DslError::CoreSchemaError {
                        source: e,
                        span: value_tok.span.clone(),
                    }
                })?;
                Annotation::Version { version }
            }
            "display" => {
                let value_tok = self.expect_string_literal()?;
                let field_str = unquote_string(&value_tok.text);
                let field = FieldName::new(&field_str).map_err(|_| {
                    DslError::InvalidFieldName {
                        name: field_str.clone(),
                        span: value_tok.span.clone(),
                    }
                })?;
                Annotation::Display { field }
            }
            other => {
                return Err(DslError::UnexpectedToken {
                    expected: "annotation name ('version' or 'display')".to_string(),
                    found: format!("'{other}'"),
                    span: name_tok.span,
                });
            }
        };

        self.expect(&Token::RParen)?;
        Ok(annotation)
    }

    /// field_def* (zero or more fields until '}')
    fn parse_fields(&mut self) -> Result<Vec<FieldDefinition>, DslError> {
        let mut fields = Vec::new();
        while self.peek_token() != Some(&Token::RBrace) && self.peek().is_some() {
            fields.push(self.parse_field()?);
        }
        Ok(fields)
    }

    /// field_def = IDENT ":" type_expr modifier*
    fn parse_field(&mut self) -> Result<FieldDefinition, DslError> {
        let name_tok = self.expect_ident("field name")?;
        let field_name = FieldName::new(&name_tok.text).map_err(|_| {
            DslError::InvalidFieldName {
                name: name_tok.text.clone(),
                span: name_tok.span.clone(),
            }
        })?;

        self.expect(&Token::Colon)?;

        let field_type = self.parse_type()?;
        let modifiers = self.parse_modifiers()?;

        if modifiers.is_empty() {
            Ok(FieldDefinition::new(field_name, field_type))
        } else {
            Ok(FieldDefinition::with_modifiers(
                field_name,
                field_type,
                modifiers,
            ))
        }
    }

    /// type_expr = relation_type | primitive_type ("[]")? | composite_type
    fn parse_type(&mut self) -> Result<FieldType, DslError> {
        match self.peek_token() {
            Some(Token::Arrow) => self.parse_relation_type(),
            Some(Token::Composite) => self.parse_composite_type(),
            _ => {
                let base_type = self.parse_primitive_type()?;
                // Check for array suffix []
                if self.peek_token() == Some(&Token::LBracket) {
                    self.advance(); // consume [
                    self.expect(&Token::RBracket)?;
                    Ok(FieldType::Array(Box::new(base_type)))
                } else {
                    Ok(base_type)
                }
            }
        }
    }

    /// primitive_type = "text" params? | "richtext" | "integer" params? | "float" params?
    ///                | "boolean" | "datetime" | "enum" "(" string_list ")" | "json"
    fn parse_primitive_type(&mut self) -> Result<FieldType, DslError> {
        let tok = self.advance().ok_or_else(|| DslError::UnexpectedEndOfInput {
            expected: "type name".to_string(),
        })?;

        match tok.token {
            Token::Text => {
                let constraints = self.parse_text_params()?;
                Ok(FieldType::Text(constraints))
            }
            Token::RichText => Ok(FieldType::RichText),
            Token::Integer => {
                let constraints = self.parse_integer_params()?;
                Ok(FieldType::Integer(constraints))
            }
            Token::Float => {
                let constraints = self.parse_float_params()?;
                Ok(FieldType::Float(constraints))
            }
            Token::Boolean => Ok(FieldType::Boolean),
            Token::DateTime => Ok(FieldType::DateTime),
            Token::Enum => self.parse_enum_type(),
            Token::Json => Ok(FieldType::Json),
            _ => Err(DslError::UnexpectedToken {
                expected: "type name (text, integer, float, boolean, datetime, enum, richtext, json, composite, or ->)"
                    .to_string(),
                found: format!("{} ('{}')", tok.token.description(), tok.text),
                span: tok.span,
            }),
        }
    }

    /// Parse optional text params: (max: N)
    fn parse_text_params(&mut self) -> Result<TextConstraints, DslError> {
        if self.peek_token() != Some(&Token::LParen) {
            return Ok(TextConstraints::unconstrained());
        }
        self.advance(); // consume (
        let params = self.parse_named_params()?;
        self.expect(&Token::RParen)?;

        let max_length = params
            .iter()
            .find(|(k, _)| k == "max")
            .map(|(_, v)| v.parse::<u32>())
            .transpose()
            .map_err(|_| {
                let span = self.current_span();
                DslError::InvalidIntegerLiteral {
                    text: "max parameter".to_string(),
                    span,
                }
            })?;

        Ok(match max_length {
            Some(max) => TextConstraints::with_max_length(max),
            None => TextConstraints::unconstrained(),
        })
    }

    /// Parse optional integer params: (min: N, max: M)
    fn parse_integer_params(&mut self) -> Result<IntegerConstraints, DslError> {
        if self.peek_token() != Some(&Token::LParen) {
            return Ok(IntegerConstraints::unconstrained());
        }
        let paren_span = self.current_span();
        self.advance(); // consume (
        let params = self.parse_named_params()?;
        self.expect(&Token::RParen)?;

        let min_val = extract_i64_param(&params, "min", &paren_span)?;
        let max_val = extract_i64_param(&params, "max", &paren_span)?;

        match (min_val, max_val) {
            (Some(min), Some(max)) => {
                if min > max {
                    Err(DslError::InvalidIntegerRange {
                        min,
                        max,
                        span: paren_span,
                    })
                } else {
                    Ok(IntegerConstraints::with_range(min, max).map_err(|e| {
                        DslError::CoreSchemaError {
                            source: e,
                            span: paren_span,
                        }
                    })?)
                }
            }
            (Some(min), None) => Ok(IntegerConstraints::with_min(min)),
            (None, Some(max)) => Ok(IntegerConstraints::with_max(max)),
            (None, None) => Ok(IntegerConstraints::unconstrained()),
        }
    }

    /// Parse optional float params: (precision: N)
    fn parse_float_params(&mut self) -> Result<FloatConstraints, DslError> {
        if self.peek_token() != Some(&Token::LParen) {
            return Ok(FloatConstraints::unconstrained());
        }
        self.advance(); // consume (
        let params = self.parse_named_params()?;
        self.expect(&Token::RParen)?;

        let precision = params
            .iter()
            .find(|(k, _)| k == "precision")
            .map(|(_, v)| v.parse::<u32>())
            .transpose()
            .map_err(|_| {
                let span = self.current_span();
                DslError::InvalidIntegerLiteral {
                    text: "precision parameter".to_string(),
                    span,
                }
            })?;

        Ok(match precision {
            Some(p) => FloatConstraints::with_precision(p),
            None => FloatConstraints::unconstrained(),
        })
    }

    /// Parse named parameters: key: value, key: value, ...
    /// Values are collected as raw strings.
    fn parse_named_params(&mut self) -> Result<Vec<(String, String)>, DslError> {
        let mut params = Vec::new();

        // Handle empty params
        if self.peek_token() == Some(&Token::RParen) {
            return Ok(params);
        }

        loop {
            let key_tok = self.expect_ident("parameter name")?;
            self.expect(&Token::Colon)?;

            let value_tok = self.advance().ok_or_else(|| DslError::UnexpectedEndOfInput {
                expected: "parameter value".to_string(),
            })?;

            let value_str = match value_tok.token {
                Token::IntegerLiteral | Token::FloatLiteral | Token::Ident => {
                    value_tok.text.clone()
                }
                Token::StringLiteral => unquote_string(&value_tok.text),
                Token::True => "true".to_string(),
                Token::False => "false".to_string(),
                _ => {
                    return Err(DslError::UnexpectedToken {
                        expected: "parameter value".to_string(),
                        found: format!(
                            "{} ('{}')",
                            value_tok.token.description(),
                            value_tok.text
                        ),
                        span: value_tok.span,
                    });
                }
            };

            params.push((key_tok.text.clone(), value_str));

            // Check for comma-separated continuation
            if self.peek_token() == Some(&Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }

        Ok(params)
    }

    /// enum_type = "enum" "(" string_list ")"
    /// The "enum" keyword has already been consumed.
    fn parse_enum_type(&mut self) -> Result<FieldType, DslError> {
        let paren_span = self.current_span();
        self.expect(&Token::LParen)?;

        let mut variants = Vec::new();
        let mut seen = HashSet::new();

        if self.peek_token() == Some(&Token::RParen) {
            let close = self.advance().unwrap();
            return Err(DslError::EmptyEnumVariants {
                span: Span::new(paren_span.start, close.span.end),
            });
        }

        loop {
            let str_tok = self.expect_string_literal()?;
            let variant = unquote_string(&str_tok.text);

            if !seen.insert(variant.clone()) {
                return Err(DslError::DuplicateEnumVariant {
                    variant,
                    span: str_tok.span,
                });
            }

            variants.push(variant);

            if self.peek_token() == Some(&Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }

        self.expect(&Token::RParen)?;

        let enum_variants = EnumVariants::new(variants).map_err(|e| {
            DslError::CoreSchemaError {
                source: e,
                span: paren_span,
            }
        })?;

        Ok(FieldType::Enum(enum_variants))
    }

    /// relation_type = "->" IDENT ("[]")?
    fn parse_relation_type(&mut self) -> Result<FieldType, DslError> {
        self.expect(&Token::Arrow)?;
        let target_tok = self.expect_ident("relation target schema name")?;
        let target = SchemaName::new(&target_tok.text).map_err(|_| {
            DslError::InvalidSchemaName {
                name: target_tok.text.clone(),
                span: target_tok.span.clone(),
            }
        })?;

        let cardinality = if self.peek_token() == Some(&Token::LBracket) {
            self.advance(); // [
            self.expect(&Token::RBracket)?;
            Cardinality::Many
        } else {
            Cardinality::One
        };

        Ok(FieldType::Relation {
            target,
            cardinality,
        })
    }

    /// composite_type = "composite" "{" field_def* "}"
    fn parse_composite_type(&mut self) -> Result<FieldType, DslError> {
        self.expect(&Token::Composite)?;
        self.expect(&Token::LBrace)?;

        let fields = self.parse_fields()?;

        self.expect(&Token::RBrace)?;

        // Validate no duplicate field names within composite
        let mut seen = HashSet::new();
        for field in &fields {
            if !seen.insert(field.name.as_str().to_string()) {
                return Err(DslError::DuplicateFieldName {
                    name: field.name.as_str().to_string(),
                    span: self.current_span(),
                });
            }
        }

        Ok(FieldType::Composite(fields))
    }

    /// modifier* (zero or more trailing modifiers)
    fn parse_modifiers(&mut self) -> Result<Vec<FieldModifier>, DslError> {
        let mut modifiers = Vec::new();

        loop {
            match self.peek_token() {
                Some(Token::Required) => {
                    self.advance();
                    modifiers.push(FieldModifier::Required);
                }
                Some(Token::Indexed) => {
                    self.advance();
                    modifiers.push(FieldModifier::Indexed);
                }
                Some(Token::Default) => {
                    self.advance();
                    let default_value = self.parse_default_value()?;
                    modifiers.push(FieldModifier::Default {
                        value: default_value,
                    });
                }
                _ => break,
            }
        }

        Ok(modifiers)
    }

    /// Parse a default value: "default" already consumed, expects "(" literal ")"
    fn parse_default_value(&mut self) -> Result<DefaultValue, DslError> {
        self.expect(&Token::LParen)?;

        let tok = self.advance().ok_or_else(|| DslError::UnexpectedEndOfInput {
            expected: "default value".to_string(),
        })?;

        let value = match tok.token {
            Token::StringLiteral => {
                DefaultValue::String(unquote_string(&tok.text))
            }
            Token::IntegerLiteral => {
                let n = parse_i64(&tok.text, &tok.span)?;
                DefaultValue::Integer(n)
            }
            Token::FloatLiteral => {
                DefaultValue::float(&tok.text).map_err(|e| DslError::CoreSchemaError {
                    source: e,
                    span: tok.span.clone(),
                })?
            }
            Token::True => DefaultValue::Boolean(true),
            Token::False => DefaultValue::Boolean(false),
            _ => {
                return Err(DslError::UnexpectedToken {
                    expected: "default value (string, integer, float, or boolean)".to_string(),
                    found: format!("{} ('{}')", tok.token.description(), tok.text),
                    span: tok.span,
                });
            }
        };

        self.expect(&Token::RParen)?;
        Ok(value)
    }

    // -- Token expectation helpers --

    fn expect_ident(&mut self, context: &str) -> Result<SpannedToken, DslError> {
        match self.advance() {
            Some(st) if st.token == Token::Ident => Ok(st),
            // Also accept keywords that can appear as identifiers in certain contexts
            // (e.g., "max", "min", "precision" as parameter names, or schema field names
            // that happen to share keyword names)
            Some(st) if is_contextual_ident(&st.token) => Ok(st),
            Some(st) => Err(DslError::UnexpectedToken {
                expected: context.to_string(),
                found: format!("{} ('{}')", st.token.description(), st.text),
                span: st.span,
            }),
            None => Err(DslError::UnexpectedEndOfInput {
                expected: context.to_string(),
            }),
        }
    }

    fn expect_string_literal(&mut self) -> Result<SpannedToken, DslError> {
        match self.advance() {
            Some(st) if st.token == Token::StringLiteral => Ok(st),
            Some(st) => Err(DslError::UnexpectedToken {
                expected: "string literal".to_string(),
                found: format!("{} ('{}')", st.token.description(), st.text),
                span: st.span,
            }),
            None => Err(DslError::UnexpectedEndOfInput {
                expected: "string literal".to_string(),
            }),
        }
    }

    fn expect_integer_literal(&mut self) -> Result<SpannedToken, DslError> {
        match self.advance() {
            Some(st) if st.token == Token::IntegerLiteral => Ok(st),
            Some(st) => Err(DslError::UnexpectedToken {
                expected: "integer literal".to_string(),
                found: format!("{} ('{}')", st.token.description(), st.text),
                span: st.span,
            }),
            None => Err(DslError::UnexpectedEndOfInput {
                expected: "integer literal".to_string(),
            }),
        }
    }
}

/// Tokens that can appear as identifiers in parameter-name contexts.
/// For example, `max`, `min`, `precision`, `version`, `display` are keywords
/// in certain contexts but valid as parameter keys or field names too.
fn is_contextual_ident(token: &Token) -> bool {
    matches!(
        token,
        Token::Text
            | Token::Integer
            | Token::Float
            | Token::Boolean
            | Token::DateTime
            | Token::Json
            | Token::Default
            | Token::Required
            | Token::Indexed
            | Token::Schema
    )
}

/// Remove surrounding quotes from a string literal and handle escape sequences.
fn unquote_string(s: &str) -> String {
    let inner = &s[1..s.len() - 1];
    let mut result = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('\\') => result.push('\\'),
                Some('"') => result.push('"'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn parse_i64(text: &str, span: &Span) -> Result<i64, DslError> {
    text.parse::<i64>().map_err(|_| DslError::InvalidIntegerLiteral {
        text: text.to_string(),
        span: span.clone(),
    })
}

fn extract_i64_param(
    params: &[(String, String)],
    key: &str,
    span: &Span,
) -> Result<Option<i64>, DslError> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| {
            v.parse::<i64>().map_err(|_| DslError::InvalidIntegerLiteral {
                text: v.clone(),
                span: span.clone(),
            })
        })
        .transpose()
}

/// Parse DSL source text into a list of schema definitions.
///
/// # Errors
///
/// Returns a list of `DslError` values if any parsing or validation errors
/// occur. The parser attempts to recover from errors and report multiple
/// issues where possible.
pub fn parse(source: &str) -> Result<Vec<SchemaDefinition>, Vec<DslError>> {
    let tokens = crate::lexer::tokenize(source)?;
    let mut parser = Parser::new(tokens);
    parser.parse_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helper --
    fn parse_one(source: &str) -> SchemaDefinition {
        let schemas = parse(source).expect("parse should succeed");
        assert_eq!(schemas.len(), 1, "expected exactly one schema");
        schemas.into_iter().next().unwrap()
    }

    // -- Basic schema parsing --

    #[test]
    fn parse_minimal_schema() {
        let schema = parse_one("schema Contact { name: text }");
        assert_eq!(schema.name.as_str(), "Contact");
        assert_eq!(schema.fields.len(), 1);
        assert_eq!(schema.fields[0].name.as_str(), "name");
        assert!(matches!(schema.fields[0].field_type, FieldType::Text(_)));
    }

    #[test]
    fn parse_multiple_fields() {
        let schema = parse_one(
            "schema Contact {
                name: text
                email: text
                active: boolean
            }",
        );
        assert_eq!(schema.fields.len(), 3);
    }

    #[test]
    fn parse_text_with_max() {
        let schema = parse_one("schema S { name: text(max: 255) }");
        match &schema.fields[0].field_type {
            FieldType::Text(c) => assert_eq!(c.max_length, Some(255)),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn parse_text_no_params() {
        let schema = parse_one("schema S { name: text }");
        match &schema.fields[0].field_type {
            FieldType::Text(c) => assert_eq!(c.max_length, None),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn parse_integer_with_range() {
        let schema = parse_one("schema S { score: integer(min: 0, max: 100) }");
        match &schema.fields[0].field_type {
            FieldType::Integer(c) => {
                assert_eq!(c.min, Some(0));
                assert_eq!(c.max, Some(100));
            }
            other => panic!("expected Integer, got {other:?}"),
        }
    }

    #[test]
    fn parse_integer_with_min_only() {
        let schema = parse_one("schema S { count: integer(min: 1) }");
        match &schema.fields[0].field_type {
            FieldType::Integer(c) => {
                assert_eq!(c.min, Some(1));
                assert_eq!(c.max, None);
            }
            other => panic!("expected Integer, got {other:?}"),
        }
    }

    #[test]
    fn parse_float_with_precision() {
        let schema = parse_one("schema S { price: float(precision: 2) }");
        match &schema.fields[0].field_type {
            FieldType::Float(c) => assert_eq!(c.precision, Some(2)),
            other => panic!("expected Float, got {other:?}"),
        }
    }

    #[test]
    fn parse_boolean() {
        let schema = parse_one("schema S { active: boolean }");
        assert!(matches!(schema.fields[0].field_type, FieldType::Boolean));
    }

    #[test]
    fn parse_datetime() {
        let schema = parse_one("schema S { created: datetime }");
        assert!(matches!(schema.fields[0].field_type, FieldType::DateTime));
    }

    #[test]
    fn parse_richtext() {
        let schema = parse_one("schema S { body: richtext }");
        assert!(matches!(schema.fields[0].field_type, FieldType::RichText));
    }

    #[test]
    fn parse_json() {
        let schema = parse_one("schema S { data: json }");
        assert!(matches!(schema.fields[0].field_type, FieldType::Json));
    }

    // -- Enum --

    #[test]
    fn parse_enum() {
        let schema = parse_one(
            r#"schema S { status: enum("active", "inactive", "pending") }"#,
        );
        match &schema.fields[0].field_type {
            FieldType::Enum(variants) => {
                assert_eq!(variants.as_slice(), &["active", "inactive", "pending"]);
            }
            other => panic!("expected Enum, got {other:?}"),
        }
    }

    #[test]
    fn parse_enum_single_variant() {
        let schema = parse_one(r#"schema S { kind: enum("only") }"#);
        match &schema.fields[0].field_type {
            FieldType::Enum(variants) => {
                assert_eq!(variants.len(), 1);
            }
            other => panic!("expected Enum, got {other:?}"),
        }
    }

    // -- Relations --

    #[test]
    fn parse_relation_one() {
        let schema = parse_one("schema S { company: -> Company }");
        match &schema.fields[0].field_type {
            FieldType::Relation {
                target,
                cardinality,
            } => {
                assert_eq!(target.as_str(), "Company");
                assert_eq!(*cardinality, Cardinality::One);
            }
            other => panic!("expected Relation, got {other:?}"),
        }
    }

    #[test]
    fn parse_relation_many() {
        let schema = parse_one("schema S { contacts: -> Contact[] }");
        match &schema.fields[0].field_type {
            FieldType::Relation {
                target,
                cardinality,
            } => {
                assert_eq!(target.as_str(), "Contact");
                assert_eq!(*cardinality, Cardinality::Many);
            }
            other => panic!("expected Relation, got {other:?}"),
        }
    }

    // -- Arrays --

    #[test]
    fn parse_text_array() {
        let schema = parse_one("schema S { tags: text[] }");
        match &schema.fields[0].field_type {
            FieldType::Array(inner) => {
                assert!(matches!(inner.as_ref(), FieldType::Text(_)));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn parse_integer_array() {
        let schema = parse_one("schema S { scores: integer[] }");
        match &schema.fields[0].field_type {
            FieldType::Array(inner) => {
                assert!(matches!(inner.as_ref(), FieldType::Integer(_)));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    // -- Composites --

    #[test]
    fn parse_composite() {
        let schema = parse_one(
            "schema S {
                address: composite {
                    street: text
                    city: text required
                    zip: text(max: 10)
                }
            }",
        );
        match &schema.fields[0].field_type {
            FieldType::Composite(fields) => {
                assert_eq!(fields.len(), 3);
                assert_eq!(fields[0].name.as_str(), "street");
                assert_eq!(fields[1].name.as_str(), "city");
                assert!(fields[1].is_required());
                assert_eq!(fields[2].name.as_str(), "zip");
            }
            other => panic!("expected Composite, got {other:?}"),
        }
    }

    // -- Modifiers --

    #[test]
    fn parse_required() {
        let schema = parse_one("schema S { name: text required }");
        assert!(schema.fields[0].is_required());
    }

    #[test]
    fn parse_indexed() {
        let schema = parse_one("schema S { email: text indexed }");
        assert!(schema.fields[0].is_indexed());
    }

    #[test]
    fn parse_multiple_modifiers() {
        let schema = parse_one("schema S { email: text required indexed }");
        assert!(schema.fields[0].is_required());
        assert!(schema.fields[0].is_indexed());
    }

    #[test]
    fn parse_default_string() {
        let schema = parse_one(r#"schema S { status: text default("active") }"#);
        let mods = &schema.fields[0].modifiers;
        assert!(matches!(
            &mods[0],
            FieldModifier::Default {
                value: DefaultValue::String(s)
            } if s == "active"
        ));
    }

    #[test]
    fn parse_default_integer() {
        let schema = parse_one("schema S { count: integer default(0) }");
        let mods = &schema.fields[0].modifiers;
        assert!(matches!(
            &mods[0],
            FieldModifier::Default {
                value: DefaultValue::Integer(0)
            }
        ));
    }

    #[test]
    fn parse_default_boolean_true() {
        let schema = parse_one("schema S { active: boolean default(true) }");
        let mods = &schema.fields[0].modifiers;
        assert!(matches!(
            &mods[0],
            FieldModifier::Default {
                value: DefaultValue::Boolean(true)
            }
        ));
    }

    #[test]
    fn parse_default_boolean_false() {
        let schema = parse_one("schema S { active: boolean default(false) }");
        let mods = &schema.fields[0].modifiers;
        assert!(matches!(
            &mods[0],
            FieldModifier::Default {
                value: DefaultValue::Boolean(false)
            }
        ));
    }

    // -- Annotations --

    #[test]
    fn parse_version_annotation() {
        let schema = parse_one("@version(2) schema S { name: text }");
        assert_eq!(schema.annotations.len(), 1);
        match &schema.annotations[0] {
            Annotation::Version { version } => assert_eq!(version.get(), 2),
            other => panic!("expected Version, got {other:?}"),
        }
    }

    #[test]
    fn parse_display_annotation() {
        let schema = parse_one(r#"@display("name") schema S { name: text }"#);
        assert_eq!(schema.annotations.len(), 1);
        match &schema.annotations[0] {
            Annotation::Display { field } => assert_eq!(field.as_str(), "name"),
            other => panic!("expected Display, got {other:?}"),
        }
    }

    #[test]
    fn parse_multiple_annotations() {
        let schema = parse_one(
            r#"@version(3)
            @display("title")
            schema S { title: text }"#,
        );
        assert_eq!(schema.annotations.len(), 2);
    }

    // -- Multiple schemas --

    #[test]
    fn parse_multiple_schemas() {
        let schemas = parse(
            "schema Contact { name: text }
             schema Company { name: text }",
        )
        .unwrap();
        assert_eq!(schemas.len(), 2);
        assert_eq!(schemas[0].name.as_str(), "Contact");
        assert_eq!(schemas[1].name.as_str(), "Company");
    }

    // -- Comments --

    #[test]
    fn parse_with_line_comments() {
        let schema = parse_one(
            "// This is a contact schema
            schema Contact {
                // The contact's name
                name: text required
            }",
        );
        assert_eq!(schema.name.as_str(), "Contact");
        assert_eq!(schema.fields.len(), 1);
    }

    #[test]
    fn parse_with_block_comments() {
        let schema = parse_one(
            "/* CRM Contact */
            schema Contact {
                name: text /* full name */ required
            }",
        );
        assert_eq!(schema.fields.len(), 1);
        assert!(schema.fields[0].is_required());
    }

    // -- Error cases --

    #[test]
    fn error_empty_schema() {
        let result = parse("schema Empty { }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(&errors[0], DslError::EmptySchema { name, .. } if name == "Empty"));
    }

    #[test]
    fn error_missing_opening_brace() {
        let result = parse("schema Contact name: text }");
        assert!(result.is_err());
    }

    #[test]
    fn error_invalid_schema_name() {
        let result = parse("schema contact { name: text }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            &errors[0],
            DslError::InvalidSchemaName { name, .. } if name == "contact"
        ));
    }

    #[test]
    fn error_duplicate_field_name() {
        let result = parse("schema S { name: text name: text }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            &errors[0],
            DslError::DuplicateFieldName { name, .. } if name == "name"
        ));
    }

    #[test]
    fn error_duplicate_annotation() {
        let result = parse("@version(1) @version(2) schema S { name: text }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            &errors[0],
            DslError::DuplicateAnnotation { kind, .. } if kind == "version"
        ));
    }

    #[test]
    fn error_empty_enum() {
        let result = parse(r#"schema S { status: enum() }"#);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(&errors[0], DslError::EmptyEnumVariants { .. }));
    }

    #[test]
    fn error_duplicate_enum_variant() {
        let result = parse(r#"schema S { status: enum("a", "b", "a") }"#);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            &errors[0],
            DslError::DuplicateEnumVariant { variant, .. } if variant == "a"
        ));
    }

    #[test]
    fn error_invalid_integer_range() {
        let result = parse("schema S { x: integer(min: 100, max: 0) }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            &errors[0],
            DslError::InvalidIntegerRange { min: 100, max: 0, .. }
        ));
    }

    #[test]
    fn error_unexpected_eof() {
        let result = parse("schema Contact {");
        assert!(result.is_err());
    }

    // -- Utility tests --

    #[test]
    fn unquote_simple() {
        assert_eq!(unquote_string(r#""hello""#), "hello");
    }

    #[test]
    fn unquote_escapes() {
        assert_eq!(unquote_string(r#""hello \"world\"""#), r#"hello "world""#);
        assert_eq!(unquote_string(r#""line1\nline2""#), "line1\nline2");
        assert_eq!(unquote_string(r#""tab\there""#), "tab\there");
    }

    #[test]
    fn unquote_empty() {
        assert_eq!(unquote_string(r#""""#), "");
    }
}
