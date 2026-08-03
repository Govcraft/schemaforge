//! The bearer credential SchemaForge presents when it calls a hook service.
//!
//! A hook service is a peer that runs under the operator's own supervision,
//! not a user. What it needs to know about an inbound RPC is that the call
//! really came from this forge — not which end user triggered it, which the
//! invocation payload already carries in its `user_id` field. So the
//! credential names the forge itself and is minted fresh per call with a short
//! lifetime, rather than being a long-lived shared secret sitting in config.
//!
//! Minting reuses the same [`PasetoGenerator`] the forge already builds for its
//! login endpoint, which means a hook service authenticates hook calls with the
//! exact `[token]` section it would use for any other acton-service surface.
//! Nothing new has to be distributed: the key material is already shared with
//! anything that validates forge-issued tokens.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use acton_service::auth::tokens::TokenGenerator;
use acton_service::middleware::token::Claims;

use super::HookError;

/// Subject claim on a minted hook credential.
///
/// A hook call is made by the forge process, so the subject names the process
/// rather than the end user whose request triggered it. Keeping the two
/// distinct matters: a hook service that authorized on `sub` would otherwise
/// see every hook call as if the end user had made it directly.
///
/// The `client:` prefix is acton-service's convention for a machine principal
/// (see [`Claims::is_client`]), so a hook service can tell a forge call from a
/// user call without knowing this constant.
pub const HOOK_CREDENTIAL_SUBJECT: &str = "client:schema-forge";

/// Role granted to a minted hook credential, so a hook service can write a
/// Cedar policy or a role check that admits the forge and nothing else.
pub const HOOK_CREDENTIAL_ROLE: &str = "schema-forge-hook-caller";

/// How long a minted hook credential is valid.
///
/// Long enough to cover the whole dispatch including a slow hook (the default
/// hook timeout is 30s), short enough that a token captured from a stalled
/// connection is useless by the time it could be replayed. It is not a session:
/// a fresh one is minted per call, so nothing depends on it outliving the RPC.
pub const HOOK_CREDENTIAL_TTL: Duration = Duration::from_secs(60);

/// Supplies the value of the `authorization` metadata key on a hook call,
/// without the `Bearer ` prefix.
///
/// Separated from the dispatcher so the transport can be tested without token
/// machinery, and so a deployment that authenticates hook calls some other way
/// (an mTLS-only mesh, say) can supply its own.
pub trait HookCredentialSource: Send + Sync + std::fmt::Debug {
    /// Produce a credential for one outbound hook call.
    ///
    /// Called once per dispatch rather than cached, so an implementation that
    /// mints short-lived tokens never hands out an expired one.
    fn bearer(&self) -> Result<String, HookError>;
}

/// A [`HookCredentialSource`] that mints a short-lived PASETO naming the forge.
#[derive(Clone)]
pub struct PasetoHookCredential<G> {
    generator: Arc<G>,
}

impl<G> std::fmt::Debug for PasetoHookCredential<G> {
    /// Deliberately opaque: the generator holds signing key material, and this
    /// type is reachable from `TonicDispatcherConfig`, which is logged.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PasetoHookCredential")
    }
}

impl<G: TokenGenerator> PasetoHookCredential<G> {
    /// Wrap a token generator. Share the generator the forge already uses for
    /// its login endpoint so hook credentials validate against the same key.
    pub fn new(generator: Arc<G>) -> Self {
        Self { generator }
    }
}

/// The claims carried by a hook credential.
///
/// Pure, so the shape of what the forge asserts about itself is testable
/// without a signing key. `exp` is filled in by the generator from the
/// requested lifetime; the zero here is a placeholder it overwrites.
pub fn hook_claims() -> Claims {
    Claims {
        sub: HOOK_CREDENTIAL_SUBJECT.to_string(),
        roles: vec![HOOK_CREDENTIAL_ROLE.to_string()],
        perms: vec![],
        exp: 0,
        iat: None,
        jti: None,
        iss: None,
        aud: None,
        email: None,
        username: None,
        custom: HashMap::new(),
    }
}

impl<G: TokenGenerator> HookCredentialSource for PasetoHookCredential<G> {
    fn bearer(&self) -> Result<String, HookError> {
        self.generator
            .generate_token_with_expiry(&hook_claims(), HOOK_CREDENTIAL_TTL)
            .map_err(|e| HookError::Internal {
                message: format!("failed to mint hook credential: {e}"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_name_the_forge_not_the_end_user() {
        let claims = hook_claims();
        assert_eq!(claims.sub, HOOK_CREDENTIAL_SUBJECT);
        assert_eq!(claims.roles, vec![HOOK_CREDENTIAL_ROLE.to_string()]);
        // An end user's identity travels in the invocation payload, never in
        // the credential — a hook service must not be able to mistake a forge
        // call for a direct call by the triggering user.
        assert!(claims.email.is_none());
        assert!(claims.username.is_none());
        // acton-service's machine-principal convention, so a hook service can
        // branch on `is_client()` rather than string-matching the subject.
        assert!(claims.is_client());
        assert!(!claims.is_user());
    }

    #[test]
    fn credential_lifetime_outlives_the_default_hook_timeout() {
        // A credential that expired mid-dispatch would fail the slowest hooks
        // and only those, which is the hardest kind of failure to diagnose.
        let default_timeout = Duration::from_millis(u64::from(super::super::DEFAULT_HOOK_TIMEOUT_MS));
        assert!(HOOK_CREDENTIAL_TTL > default_timeout);
    }
}
