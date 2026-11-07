use core::fmt;

use subtle::ConstantTimeEq;

use super::CryptoProvider;

/// Certificate fingerprint.
///
/// DTLS uses self signed certificates, and the fingerprint is communicated via
/// SDP to let the remote peer verify who is connecting.
#[derive(Debug, Clone)]
pub struct Fingerprint {
    /// Hash function used to produce the `bytes`.
    ///
    /// This is normally `sha-256`.
    pub hash_func: String,

    /// Digest of the certificate by the algorithm in `hash_func`.
    pub bytes: Vec<u8>,
}

impl Fingerprint {
    /// Prefer SHA-256 to SHA-1, and supported algorithms to unsupported ones.
    /// Lower values mean higher preference (RFC 8122 §5.1).
    pub(crate) fn hash_preference(&self) -> u8 {
        if self.hash_func.eq_ignore_ascii_case("sha-256") {
            0
        } else if self.hash_func.eq_ignore_ascii_case("sha-1") {
            1
        } else {
            2
        }
    }

    /// The fingerprint of a DER certificate made with `hash_func`, the hash
    /// function the remote SDP names (RFC 8122 §5, case-insensitive). Only
    /// `sha-256` and `sha-1` are supported; `None` for any other.
    ///
    /// The result keeps `hash_func` as given, so it compares equal to the
    /// remote fingerprint when the digests match.
    pub(crate) fn of_certificate(
        der: &[u8],
        hash_func: &str,
        provider: &CryptoProvider,
    ) -> Option<Fingerprint> {
        let bytes = if hash_func.eq_ignore_ascii_case("sha-256") {
            provider.sha256_provider.sha256(der).to_vec()
        } else if hash_func.eq_ignore_ascii_case("sha-1") {
            provider.sha1_provider.sha1(der).to_vec()
        } else {
            return None;
        };
        Some(Fingerprint {
            hash_func: hash_func.to_owned(),
            bytes,
        })
    }
}

impl PartialEq for Fingerprint {
    fn eq(&self, other: &Self) -> bool {
        // Use constant-time comparison for the bytes to prevent timing attacks
        self.hash_func == other.hash_func && self.bytes[..].ct_eq(&other.bytes[..]).into()
    }
}

impl Eq for Fingerprint {}

impl std::hash::Hash for Fingerprint {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.hash_func.hash(state);
        self.bytes.hash(state);
    }
}

// DO NOT CHANGE!
// This format is exactly what's needed in n SDP.
impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ", self.hash_func)?;
        for (i, b) in self.bytes.iter().enumerate() {
            if i > 0 {
                write!(f, ":")?;
            }
            write!(f, "{:02X}", b)?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Fingerprint {
    type Err = String;

    fn from_str(hex_string: &str) -> Result<Self, Self::Err> {
        let (hash_func, hex_with_colons) = hex_string
            .split_once(' ')
            .ok_or_else(|| "Failed to split once".to_owned())?;

        let mut bytes = Vec::new();
        for hex in hex_with_colons.split(':') {
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|e| format!("Failed to parse fingerprint: {}", e))?;
            bytes.push(byte);
        }

        Ok(Self {
            hash_func: hash_func.to_owned(),
            bytes,
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn fingerprint_format() {
        let f = Fingerprint {
            hash_func: "foo".to_string(),
            bytes: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17],
        };

        assert_eq!(
            f.to_string(),
            "foo 00:01:02:03:04:05:06:07:08:09:0A:0B:0C:0D:0E:0F:10:11"
        );
    }
}
