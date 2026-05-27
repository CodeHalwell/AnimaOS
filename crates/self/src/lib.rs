#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

//! Self/Non-Self barrier: typestate-encoded object-capability tokens.
//!
//! A capability is created in the `Unverified` state. Calling [`Capability::verify`]
//! consumes the unverified token and produces a `Verified` token that can be
//! presented at trust boundaries.

use core::marker::PhantomData;

/// Marker indicating the capability has not been verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unverified;

/// Marker indicating the capability has been verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified;

/// Object-capability token used for least-privilege access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability<State> {
    /// Subject identity.
    pub uid: u32,
    /// Group identity.
    pub gid: u32,
    /// Capability name.
    pub capability: &'static str,
    _state: PhantomData<State>,
}

impl Capability<Unverified> {
    /// Creates a new unverified capability token.
    pub fn new(uid: u32, gid: u32, capability: &'static str) -> Self {
        Self {
            uid,
            gid,
            capability,
            _state: PhantomData,
        }
    }

    /// Consumes self and produces a verified capability if `policy` accepts it.
    pub fn verify<F>(self, policy: F) -> Result<Capability<Verified>, CapabilityError>
    where
        F: FnOnce(&Capability<Unverified>) -> bool,
    {
        if policy(&self) {
            Ok(Capability {
                uid: self.uid,
                gid: self.gid,
                capability: self.capability,
                _state: PhantomData,
            })
        } else {
            Err(CapabilityError::Denied)
        }
    }
}

/// Errors produced when working with capability tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityError {
    /// Policy rejected the verification request.
    Denied,
}

/// Backwards-compatible alias for the previous flat type.
pub type CapabilityToken = Capability<Verified>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_promotes_unverified_to_verified() {
        let token = Capability::<Unverified>::new(1000, 1000, "tool.dispatch");
        let verified = token.verify(|t| t.capability == "tool.dispatch").unwrap();
        assert_eq!(verified.uid, 1000);
        assert_eq!(verified.capability, "tool.dispatch");
    }

    #[test]
    fn verify_can_reject() {
        let token = Capability::<Unverified>::new(0, 0, "kernel.reboot");
        let err = token.verify(|_| false).unwrap_err();
        assert_eq!(err, CapabilityError::Denied);
    }
}
