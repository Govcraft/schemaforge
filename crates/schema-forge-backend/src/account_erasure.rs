//! Aggregate evidence returned only after account erasure commits.

/// Related records removed together with a User, without retaining their data.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountErasureCounts {
    /// External sign-in identities removed.
    pub identities: u64,
    /// Tenant memberships removed.
    pub memberships: u64,
    /// Invitations for the stored account email removed, regardless of status.
    pub invitations: u64,
}
