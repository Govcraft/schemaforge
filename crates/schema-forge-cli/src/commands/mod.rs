#[cfg(feature = "server")]
pub mod apply;
#[cfg(feature = "server")]
pub mod bootstrap_admin;
pub mod codegen;
pub mod completions;
#[cfg(feature = "server")]
pub mod entity;
#[cfg(feature = "server")]
pub mod export;
pub mod hooks;
pub mod init;
#[cfg(feature = "server")]
pub mod inspect;
#[cfg(feature = "server")]
pub mod login;
#[cfg(feature = "server")]
pub mod migrate;
pub mod parse;
#[cfg(feature = "server")]
pub mod policies;
#[cfg(feature = "server")]
mod policy_preflight;
#[cfg(feature = "server")]
mod schema_update;
#[cfg(feature = "server")]
pub mod serve;
#[cfg(feature = "embedded-console")]
pub mod serve_console;
pub mod sign;
pub mod site;
#[cfg(feature = "server")]
pub mod token;
pub mod trust_bundle;
pub mod verify;

#[cfg(feature = "server")]
mod backend;
#[cfg(feature = "server")]
pub use backend::{connect_backend, connect_backend_read_only};
