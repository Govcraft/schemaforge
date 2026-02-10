//! # schema-forge-dsl
//!
//! DSL parser and printer for the SchemaForge schema definition language.
//!
//! This crate provides:
//! - A lexer that tokenizes `.schema` source files
//! - A recursive descent parser that produces `SchemaDefinition` values
//! - A printer that converts `SchemaDefinition` back to DSL text
//! - Round-trip fidelity: `parse(print(schema))` produces an equivalent AST
//!
//! # Example
//!
//! ```
//! use schema_forge_dsl::{parse, print};
//!
//! let source = r#"
//! schema Contact {
//!     name: text(max: 255) required
//!     email: text required indexed
//!     active: boolean default(true)
//! }
//! "#;
//!
//! let schemas = parse(source).expect("parse failed");
//! assert_eq!(schemas.len(), 1);
//! assert_eq!(schemas[0].name.as_str(), "Contact");
//!
//! let dsl_text = print(&schemas[0]);
//! assert!(dsl_text.contains("schema Contact {"));
//! ```

pub mod error;
mod lexer;
pub mod parser;
pub mod printer;
pub mod token;

pub use error::{DslError, Span};
pub use parser::parse;
pub use printer::{print, print_all};
