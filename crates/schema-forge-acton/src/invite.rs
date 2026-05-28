//! Invite-token minting and verification (issue #71).
//!
//! An invitation is carried as a PASETO minted with the **same** generator and
//! key the login endpoint uses, so it inherits the deployment's existing key
//! management and rotation story. What distinguishes it from a session bearer
//! is a `purpose = "invite"` custom claim: [`verify_invite_token`] refuses any
//! token lacking it, and the bearer-auth middleware would likewise reject an
//! invite token presented as a credential because no route grants access on a
//! `purpose=invite` claim. That hard separation is the whole point — an invite
//! link can never be replayed as an API session, nor a leaked session minted
//! into an invite.
//!
//! The emailed accept link carries only the opaque, high-entropy `invite_id`.
//! The full token is persisted in the invite store and *reconstructed* on
//! accept, where it is re-verified here so the invitee's role and tenant are
//! read from the **signed** claims rather than from mutable database columns —
//! defense-in-depth against tampering with the stored row.

use std::time::Duration;

use acton_service::auth::tokens::paseto_generator::PasetoGenerator;
use acton_service::auth::tokens::{ClaimsBuilder, TokenGenerator};
use acton_service::middleware::token::TokenValidator;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Custom-claim value that marks a token as an invitation rather than a
/// session credential.
pub const INVITE_PURPOSE: &str = "invite";

const CLAIM_PURPOSE: &str = "purpose";
const CLAIM_INVITE_ID: &str = "invite_id";
const CLAIM_EMAIL: &str = "invite_email";
const CLAIM_TENANT_TYPE: &str = "invite_tenant_type";
const CLAIM_TENANT_ID: &str = "invite_tenant_id";
const CLAIM_ROLE: &str = "invite_role";

/// Parameters baked into a freshly minted invite token.
#[derive(Debug, Clone)]
pub struct InviteTokenParams {
    /// Invitee email — becomes the future `User.email` and the token subject.
    pub email: String,
    /// Tenant root type the invitee is being added to (e.g. `"Organization"`).
    pub tenant_type: Option<String>,
    /// Tenant root entity id.
    pub tenant_id: Option<String>,
    /// Role granted within that tenant.
    pub role: Option<String>,
}

/// Result of minting: the opaque id to email, the full token to persist, and
/// the absolute expiry to store.
#[derive(Debug, Clone)]
pub struct MintedInvite {
    /// High-entropy opaque reference emailed to the invitee.
    pub invite_id: String,
    /// The full PASETO; persisted in the invite store, reconstructed on accept.
    pub token: String,
    /// Absolute expiry (mirrors the token's `exp`).
    pub expires_at: DateTime<Utc>,
}

/// The trusted contents of a verified invite token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedInvite {
    pub invite_id: String,
    pub email: String,
    pub tenant_type: Option<String>,
    pub tenant_id: Option<String>,
    pub role: Option<String>,
}

/// Errors from minting or verifying an invite token.
#[derive(Debug, thiserror::Error)]
pub enum InviteTokenError {
    /// The token could not be minted (claims build or generator failure).
    #[error("failed to mint invite token: {0}")]
    Mint(String),
    /// The token failed cryptographic validation or has expired.
    #[error("invite token is invalid or expired: {0}")]
    Invalid(String),
    /// The token verified but is not an invitation (missing/incorrect purpose).
    #[error("token is not an invitation")]
    WrongPurpose,
    /// A required claim was absent from an otherwise valid invite token.
    #[error("invite token is missing the '{0}' claim")]
    MissingClaim(&'static str),
}

/// Mint an invitation token. `ttl` sets both the PASETO `exp` and the returned
/// [`MintedInvite::expires_at`]. The `invite_id` is a fresh v4 UUID (122 bits
/// of entropy), unguessable so possession of the emailed link is the only way
/// to present a valid reference.
pub fn mint_invite_token(
    generator: &PasetoGenerator,
    params: &InviteTokenParams,
    ttl: Duration,
) -> Result<MintedInvite, InviteTokenError> {
    let invite_id = Uuid::new_v4().to_string();

    let mut builder = ClaimsBuilder::new()
        .subject(format!("invite:{}", params.email))
        .custom_claim(CLAIM_PURPOSE, INVITE_PURPOSE.to_string())
        .custom_claim(CLAIM_INVITE_ID, invite_id.clone())
        .custom_claim(CLAIM_EMAIL, params.email.clone());
    if let Some(tt) = params.tenant_type.as_deref().filter(|s| !s.is_empty()) {
        builder = builder.custom_claim(CLAIM_TENANT_TYPE, tt.to_string());
    }
    if let Some(ti) = params.tenant_id.as_deref().filter(|s| !s.is_empty()) {
        builder = builder.custom_claim(CLAIM_TENANT_ID, ti.to_string());
    }
    if let Some(role) = params.role.as_deref().filter(|s| !s.is_empty()) {
        builder = builder.custom_claim(CLAIM_ROLE, role.to_string());
    }

    let claims = builder
        .build()
        .map_err(|e| InviteTokenError::Mint(e.to_string()))?;
    let token = generator
        .generate_token_with_expiry(&claims, ttl)
        .map_err(|e| InviteTokenError::Mint(e.to_string()))?;

    let expires_at = Utc::now() + chrono::Duration::seconds(ttl.as_secs() as i64);
    Ok(MintedInvite {
        invite_id,
        token,
        expires_at,
    })
}

/// Verify an invite token: validates the signature and expiry through the
/// supplied [`TokenValidator`], then enforces `purpose = "invite"` and extracts
/// the trusted invite parameters from the signed claims.
pub fn verify_invite_token<V: TokenValidator>(
    validator: &V,
    token: &str,
) -> Result<VerifiedInvite, InviteTokenError> {
    let claims = validator
        .validate_token(token)
        .map_err(|e| InviteTokenError::Invalid(e.to_string()))?;

    if claim_str(&claims, CLAIM_PURPOSE).as_deref() != Some(INVITE_PURPOSE) {
        return Err(InviteTokenError::WrongPurpose);
    }

    let invite_id =
        claim_str(&claims, CLAIM_INVITE_ID).ok_or(InviteTokenError::MissingClaim("invite_id"))?;
    let email =
        claim_str(&claims, CLAIM_EMAIL).ok_or(InviteTokenError::MissingClaim("invite_email"))?;

    Ok(VerifiedInvite {
        invite_id,
        email,
        tenant_type: claim_str(&claims, CLAIM_TENANT_TYPE),
        tenant_id: claim_str(&claims, CLAIM_TENANT_ID),
        role: claim_str(&claims, CLAIM_ROLE),
    })
}

fn claim_str(claims: &acton_service::middleware::token::Claims, key: &str) -> Option<String> {
    claims
        .custom_claim(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use acton_service::auth::config::TokenGenerationConfig;
    use acton_service::config::PasetoConfig;
    use acton_service::middleware::paseto::PasetoAuth;
    use std::io::Write;
    use std::path::PathBuf;

    /// Write a fresh 32-byte symmetric key to a temp file and return a
    /// matched (generator, validator) pair plus the key path for cleanup.
    fn key_pair() -> (PasetoGenerator, PasetoAuth, PathBuf) {
        let mut key = [0u8; 32];
        // Deterministic-but-arbitrary fill; the value is irrelevant, only that
        // the generator and validator share it. (Test-only.)
        for (i, b) in key.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        let path = std::env::temp_dir().join(format!("sf-invite-test-{}.key", Uuid::new_v4()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&key).unwrap();
        f.flush().unwrap();

        let gen_cfg = TokenGenerationConfig {
            access_token_lifetime_secs: 3600,
            issuer: Some("schemaforge-test".to_string()),
            audience: None,
            include_jti: true,
        };
        let generator = PasetoGenerator::with_symmetric_key(key, gen_cfg);
        let validator = PasetoAuth::new(&PasetoConfig {
            version: "v4".to_string(),
            purpose: "local".to_string(),
            key_path: path.clone(),
            issuer: None,
            audience: None,
            public_paths: vec![],
        })
        .unwrap();
        (generator, validator, path)
    }

    fn params() -> InviteTokenParams {
        InviteTokenParams {
            email: "invitee@example.gov".to_string(),
            tenant_type: Some("Organization".to_string()),
            tenant_id: Some("org_abc".to_string()),
            role: Some("member".to_string()),
        }
    }

    #[test]
    fn mint_then_verify_roundtrips() {
        let (generator, validator, path) = key_pair();
        let minted = mint_invite_token(&generator, &params(), Duration::from_secs(3600)).unwrap();
        assert!(!minted.invite_id.is_empty());
        assert!(minted.expires_at > Utc::now());

        let verified = verify_invite_token(&validator, &minted.token).unwrap();
        assert_eq!(verified.invite_id, minted.invite_id);
        assert_eq!(verified.email, "invitee@example.gov");
        assert_eq!(verified.tenant_type.as_deref(), Some("Organization"));
        assert_eq!(verified.tenant_id.as_deref(), Some("org_abc"));
        assert_eq!(verified.role.as_deref(), Some("member"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn non_invite_token_is_rejected() {
        let (generator, validator, path) = key_pair();
        // A login-shaped token with no purpose=invite claim.
        let claims = ClaimsBuilder::new()
            .subject("user:someone")
            .role("admin")
            .build()
            .unwrap();
        let token = generator
            .generate_token_with_expiry(&claims, Duration::from_secs(3600))
            .unwrap();
        assert!(matches!(
            verify_invite_token(&validator, &token),
            Err(InviteTokenError::WrongPurpose)
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn garbage_token_is_invalid() {
        let (_generator, validator, path) = key_pair();
        assert!(matches!(
            verify_invite_token(&validator, "v4.local.not-a-real-token"),
            Err(InviteTokenError::Invalid(_))
        ));
        let _ = std::fs::remove_file(path);
    }
}
