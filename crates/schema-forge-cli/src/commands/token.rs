use acton_service::auth::config::{PasetoGenerationConfig, TokenGenerationConfig};
use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use acton_service::auth::tokens::{ClaimsBuilder, TokenGenerator};

use crate::cli::{GenerateTokenArgs, InitKeyArgs, TokenCommands};
use crate::error::CliError;
use crate::output::OutputContext;

/// Run token subcommands.
pub async fn run(command: TokenCommands, output: &OutputContext) -> Result<(), CliError> {
    match command {
        TokenCommands::InitKey(args) => init_key(args, output),
        TokenCommands::Generate(args) => generate(args, output),
    }
}

/// Generate a 32-byte random PASETO V4 symmetric key.
fn init_key(args: InitKeyArgs, output: &OutputContext) -> Result<(), CliError> {
    let path = &args.output;

    if path.exists() {
        return Err(CliError::Config {
            message: format!("key file already exists at {}", path.display()),
        });
    }

    write_new_paseto_key(path)?;

    output.success(&format!("PASETO key written to {}", path.display()));
    output.status("  Add to .gitignore: keys/");
    Ok(())
}

/// Ensure a PASETO V4 symmetric key exists at `path`.
///
/// If the file already exists this is a no-op. Otherwise, a fresh 32-byte
/// random key is written and (on Unix) locked down to `0o600`.
///
/// Used by `schema-forge serve` to make the login endpoint work out of the
/// box with a stock `config.toml` and no manual key provisioning.
pub fn ensure_paseto_key(path: &std::path::Path) -> Result<(), CliError> {
    if path.exists() {
        return Ok(());
    }
    write_new_paseto_key(path)
}

/// Write a fresh 32-byte random PASETO V4 key to `path`.
///
/// Caller is responsible for checking that the path does not already exist
/// when overwrite protection is desired.
fn write_new_paseto_key(path: &std::path::Path) -> Result<(), CliError> {
    // Create parent directories
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| CliError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }

    // Generate 32 random bytes from OS entropy
    use std::io::Read;
    let mut key = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut key))
        .map_err(|e| CliError::Io {
            path: std::path::PathBuf::from("/dev/urandom"),
            source: e,
        })?;
    std::fs::write(path, key).map_err(|e| CliError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Restrict permissions on Unix
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

/// Generate a PASETO token from the given key and claims.
fn generate(args: GenerateTokenArgs, output: &OutputContext) -> Result<(), CliError> {
    let key_path = &args.key;

    if !key_path.exists() {
        return Err(CliError::Config {
            message: format!(
                "key file not found at {}. Run `schema-forge token init-key` first.",
                key_path.display()
            ),
        });
    }

    let paseto_config = PasetoGenerationConfig {
        version: "v4".to_string(),
        purpose: "local".to_string(),
        key_path: key_path.clone(),
        issuer: Some(args.issuer.clone()),
        audience: None,
    };

    let token_config = TokenGenerationConfig {
        access_token_lifetime_secs: args.lifetime,
        issuer: Some(args.issuer),
        audience: None,
        include_jti: true,
    };

    let generator =
        PasetoGenerator::new(&paseto_config, &token_config).map_err(|e| CliError::Config {
            message: format!("failed to initialize token generator: {e}"),
        })?;

    let mut builder = ClaimsBuilder::new().subject(&args.sub);

    for role in &args.roles {
        builder = builder.role(role);
    }

    // Parse tenant chain JSON if provided
    if let Some(ref tc_json) = args.tenant_chain {
        let tenant_chain: serde_json::Value =
            serde_json::from_str(tc_json).map_err(|e| CliError::Config {
                message: format!("invalid --tenant-chain JSON: {e}"),
            })?;
        builder = builder.custom_claim("tenant_chain", tenant_chain);
    }

    let claims = builder.build().map_err(|e| CliError::Config {
        message: format!("failed to build claims: {e}"),
    })?;

    let token = generator
        .generate_token(&claims)
        .map_err(|e| CliError::Config {
            message: format!("failed to generate token: {e}"),
        })?;

    output.success("Token generated:");
    // Print the raw token so it can be piped
    println!("{token}");

    Ok(())
}
