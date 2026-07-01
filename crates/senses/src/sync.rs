//! Std/no_std lock compatibility shim.
//!
//! The afferent path ([`crate::SensoryBridge`]) and its consumers (`vita`'s
//! lifecycle manager and audit log) share state behind `Arc<Mutex<…>>`. Under
//! `std` that is [`std::sync::Mutex`]; in the bare-metal microVM kernel there
//! is no OS to park a thread on, so a [`spin::Mutex`] stands in.
//!
//! The shim preserves the *call shape* of the std API — `lock()` returns a
//! `Result` whose error type can never be constructed — so shared code can
//! keep writing `lock().unwrap()` (or `.expect(…)`) and compile identically
//! on both targets. Poisoning does not exist in the spin path: a panic while
//! holding the lock halts the kernel anyway (the panic handler never
//! unwinds), so there is no poisoned state to observe.
//!
//! This lives in `senses` because it is the lowest crate on the somatic spine
//! that needs it; promote it to a dedicated crate if a third consumer appears
//! outside the `senses` → `vita` chain.

#[cfg(feature = "std")]
pub use std::sync::{Mutex, MutexGuard};

#[cfg(not(feature = "std"))]
pub use no_std_impl::{Mutex, MutexGuard};

/// Acquire `mutex`, recovering the guard even if a previous holder panicked
/// while holding it.
///
/// On `std` a poisoned [`std::sync::Mutex`] is recovered via
/// [`std::sync::PoisonError::into_inner`], so a single recoverable panic cannot
/// permanently brick every subsequent lock on the always-on somatic path
/// (VITA-2 / CORE-6). The critical sections guarded this way only push/pop or
/// swap owned state, so observing a value left by a panicking section is safe.
/// On the spin path locking never fails, so this is an infallible unwrap of an
/// uninhabited error.
#[cfg(feature = "std")]
pub fn lock_recover<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// See the `std` variant above. On the bare-metal spin path the lock cannot be
/// poisoned, so the `Err` arm is uninhabited.
#[cfg(not(feature = "std"))]
pub fn lock_recover<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
    }
}

#[cfg(not(feature = "std"))]
mod no_std_impl {
    use core::fmt;
    use core::ops::{Deref, DerefMut};

    /// Error type for [`Mutex::lock`] that can never be constructed — the
    /// spin lock cannot be poisoned.
    pub enum Never {}

    impl fmt::Debug for Never {
        fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
            match *self {}
        }
    }

    /// A spin-backed mutex exposing the `std::sync::Mutex` call shape.
    pub struct Mutex<T: ?Sized>(spin::Mutex<T>);

    /// Guard mirroring `std::sync::MutexGuard`.
    pub struct MutexGuard<'a, T: ?Sized>(spin::MutexGuard<'a, T>);

    impl<T> Mutex<T> {
        /// Create a new lock around `value`.
        pub fn new(value: T) -> Self {
            Self(spin::Mutex::new(value))
        }
    }

    impl<T: ?Sized> Mutex<T> {
        /// Acquire the lock, spinning until it is available.
        ///
        /// Always returns `Ok`: there is no poisoning in the spin path, but
        /// the `Result` keeps `lock().unwrap()` call sites source-compatible
        /// with `std::sync::Mutex`.
        #[allow(clippy::missing_errors_doc)]
        pub fn lock(&self) -> Result<MutexGuard<'_, T>, Never> {
            Ok(MutexGuard(self.0.lock()))
        }
    }

    impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.0.try_lock() {
                Some(guard) => f.debug_tuple("Mutex").field(&&*guard).finish(),
                None => f.write_str("Mutex(<locked>)"),
            }
        }
    }

    impl<T: ?Sized> Deref for MutexGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.0
        }
    }

    impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            &mut self.0
        }
    }
}
