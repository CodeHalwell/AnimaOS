#![forbid(unsafe_code)]

/// Object-capability token used for least-privilege access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityToken {
    /// Subject identity.
    pub uid: u32,
    /// Group identity.
    pub gid: u32,
    /// Capability name.
    pub capability: &'static str,
}
