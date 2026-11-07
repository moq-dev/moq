//! PII-safe wrapper for sensitive values.
//!
//! The `Pii<T>` type is a generic wrapper for any value that may contain
//! personally identifiable information (PII) or other sensitive data.
//! When the `pii` feature is enabled, any value wrapped in `Pii` will be
//! redacted (displayed as `REDACTED`) when formatted with `Display`.
//! Otherwise, the inner value is shown as normal. This helps prevent
//! accidental leakage of sensitive information in logs or user-facing
//! output.
//!
//! Logging or displaying sensitive data such as IP addresses, user IDs, or
//! authentication tokens can lead to privacy violations or security
//! incidents.
//!
//! This wrapper should be used for debug, info, warn, and error logs.
//! It is not intended for trace-level logs, as trace logs are typically
//! disabled in production environments.

use core::fmt;
use core::ops::Deref;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pii<T>(pub T);

impl<T: fmt::Display> fmt::Display for Pii<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(feature = "pii")]
        {
            write!(f, "{{REDACTED}}")
        }
        #[cfg(not(feature = "pii"))]
        {
            write!(f, "{}", self.0)
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Pii<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(feature = "pii")]
        {
            write!(f, "{{REDACTED}}")
        }
        #[cfg(not(feature = "pii"))]
        {
            write!(f, "{:?}", self.0)
        }
    }
}

impl<T> Deref for Pii<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn socket_addr_display() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let pii_addr = Pii(addr);

        #[cfg(feature = "pii")]
        assert_eq!(pii_addr.to_string(), "{REDACTED}");

        #[cfg(not(feature = "pii"))]
        assert_eq!(pii_addr.to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn string_display() {
        let sensitive = String::from("sensitive data");
        let pii_string = Pii(sensitive);

        #[cfg(feature = "pii")]
        assert_eq!(pii_string.to_string(), "{REDACTED}");

        #[cfg(not(feature = "pii"))]
        assert_eq!(pii_string.to_string(), "sensitive data");
    }
}
