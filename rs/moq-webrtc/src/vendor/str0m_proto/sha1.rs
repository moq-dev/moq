use std::fmt::Debug;

/// Marker trait for types that are safe to use in crypto provider components.
///
/// This trait combines the common bounds required for crypto provider trait objects:
/// - [`Send`] + [`Sync`]: Thread-safe
/// - [`Debug`]: Support debugging
///
/// Note: We don't require `UnwindSafe` because some error types (like `dimpl::Error`)
/// may not implement it, but they're still safe to use in our context.
pub trait CryptoSafe: Send + Sync + Debug {}

/// Blanket implementation: any type satisfying the bounds implements [`CryptoSafe`].
impl<T: Send + Sync + Debug> CryptoSafe for T {}

/// SHA1 HMAC provider for STUN message integrity.
pub trait Sha1HmacProvider: CryptoSafe {
    /// Compute HMAC-SHA1(key, payloads) and return the result.
    fn sha1_hmac(&self, key: &[u8], payloads: &[&[u8]]) -> [u8; 20];
}
