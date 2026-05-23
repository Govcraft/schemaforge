//! `schemaforge login` — credential flow against a running instance.
//!
//! Calls `POST /auth/login`, prompts for the password (or reads
//! `--password-stdin`), and by default caches the returned Bearer token to
//! `$XDG_STATE_HOME/schemaforge/token` (0600) so subsequent `entity` commands
//! pick it up automatically. Distinct from `token generate`, which mints a
//! token offline from the PASETO key.

use std::path::Path;

use serde_json::Value;

use crate::cli::{GlobalOpts, LoginArgs};
use crate::config::{load_svc_config, resolve_client_config, xdg_state_token_path};
use crate::error::CliError;
use crate::http::ForgeClient;
use crate::output::OutputContext;
use crate::progress;

/// Authenticate and (by default) cache the token.
pub async fn run(
    args: LoginArgs,
    global: &GlobalOpts,
    output: &OutputContext,
) -> Result<(), CliError> {
    let password = read_password(&args)?;

    let svc = load_svc_config(global)?;
    let rc = resolve_client_config(&svc, &args.conn)?;
    let client = ForgeClient::new(&rc, output)?;

    let spinner = output
        .show_progress()
        .then(|| progress::create_spinner("Authenticating…"));
    let result = client.login(&args.username, &password).await;
    if let Some(sp) = spinner {
        match &result {
            Ok(_) => sp.finish_and_clear(),
            Err(e) => progress::finish_spinner_error(&sp, &e.to_string()),
        }
    }
    let value = result?;

    let token = value
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Server {
            message: "login response did not contain a token".to_string(),
        })?;
    let expires_at = value.get("expires_at").and_then(Value::as_str);
    let roles = collect_roles(&value);

    if !args.no_save {
        let path = xdg_state_token_path();
        write_token_cache(&path, token)?;
        output.status(&format!("token cached at {}", path.display()));
    }

    if args.print_token && output.mode != crate::output::OutputMode::Json {
        // Raw token to stdout for piping (json mode already includes it).
        println!("{token}");
    }

    if output.mode == crate::output::OutputMode::Json {
        output.print_json(&value);
    } else {
        let suffix = expires_at
            .map(|e| format!(" (token expires {e})"))
            .unwrap_or_default();
        output.success(&format!("authenticated as {}{suffix}", args.username));
        if !roles.is_empty() {
            output.status(&format!("roles: {}", roles.join(", ")));
        }
    }

    Ok(())
}

/// Read the password from stdin (`--password-stdin`) or an interactive
/// prompt. Never accepted as a flag.
fn read_password(args: &LoginArgs) -> Result<String, CliError> {
    if args.password_stdin {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| CliError::Io {
                path: std::path::PathBuf::from("<stdin>"),
                source: e,
            })?;
        return Ok(buf.trim_end_matches(['\n', '\r']).to_string());
    }
    dialoguer::Password::new()
        .with_prompt(format!("Password for {}", args.username))
        .interact()
        .map_err(|_| CliError::Config {
            message: "could not read password interactively; use --password-stdin in \
                      non-interactive contexts"
                .to_string(),
        })
}

/// Extract the `roles` array from a login response as plain strings.
fn collect_roles(value: &Value) -> Vec<String> {
    value
        .get("roles")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Write the token to the cache path, creating parents and restricting
/// permissions to 0600 on Unix.
fn write_token_cache(path: &Path, token: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CliError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    std::fs::write(path, token).map_err(|e| CliError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(|e| CliError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collect_roles_reads_string_array() {
        let v = json!({ "roles": ["admin", "staff"] });
        assert_eq!(collect_roles(&v), vec!["admin", "staff"]);
    }

    #[test]
    fn collect_roles_missing_is_empty() {
        assert!(collect_roles(&json!({})).is_empty());
        assert!(collect_roles(&json!({ "roles": "nope" })).is_empty());
    }

    #[test]
    fn write_token_cache_sets_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("token");
        write_token_cache(&path, "v4.local.abc").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v4.local.abc");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
