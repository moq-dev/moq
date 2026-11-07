//! Key exchange group implementations using RustCrypto.

use p256::{PublicKey as P256PublicKey, ecdh::EphemeralSecret, elliptic_curve::Generate};
use p384::{PublicKey as P384PublicKey, ecdh::EphemeralSecret as P384EphemeralSecret};

use super::super::{ActiveKeyExchange, SupportedKxGroup};
use crate::CryptoError;
use crate::buffer::Buf;
use crate::types::NamedGroup;

/// ECDHE key exchange implementation.
enum EcdhKeyExchange {
    X25519 {
        secret: x25519_dalek::EphemeralSecret,
        public_key: Buf,
    },
    P256 {
        secret: EphemeralSecret,
        public_key: Buf,
    },
    P384 {
        secret: P384EphemeralSecret,
        public_key: Buf,
    },
}

impl std::fmt::Debug for EcdhKeyExchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EcdhKeyExchange::X25519 { public_key, .. } => f
                .debug_struct("EcdhKeyExchange::X25519")
                .field("public_key_len", &public_key.len())
                .finish_non_exhaustive(),
            EcdhKeyExchange::P256 { public_key, .. } => f
                .debug_struct("EcdhKeyExchange::P256")
                .field("public_key_len", &public_key.len())
                .finish_non_exhaustive(),
            EcdhKeyExchange::P384 { public_key, .. } => f
                .debug_struct("EcdhKeyExchange::P384")
                .field("public_key_len", &public_key.len())
                .finish_non_exhaustive(),
        }
    }
}

impl EcdhKeyExchange {
    fn new(group: NamedGroup, mut buf: Buf) -> Result<Self, CryptoError> {
        match group {
            NamedGroup::X25519 => {
                let secret = x25519_dalek::EphemeralSecret::random();
                let public_key_obj = x25519_dalek::PublicKey::from(&secret);
                buf.clear();
                buf.extend_from_slice(public_key_obj.as_bytes());
                Ok(EcdhKeyExchange::X25519 {
                    secret,
                    public_key: buf,
                })
            }
            NamedGroup::Secp256r1 => {
                let secret = EphemeralSecret::generate();
                let public_key_obj = P256PublicKey::from(&secret);
                let public_key_bytes = public_key_obj.to_sec1_bytes();
                buf.clear();
                buf.extend_from_slice(&public_key_bytes);
                Ok(EcdhKeyExchange::P256 {
                    secret,
                    public_key: buf,
                })
            }
            NamedGroup::Secp384r1 => {
                let secret = P384EphemeralSecret::generate();
                let public_key_obj = P384PublicKey::from(&secret);
                let public_key_bytes = public_key_obj.to_sec1_bytes();
                buf.clear();
                buf.extend_from_slice(&public_key_bytes);
                Ok(EcdhKeyExchange::P384 {
                    secret,
                    public_key: buf,
                })
            }
            _ => Err(CryptoError::UnsupportedKeyExchangeGroup(group)),
        }
    }
}

impl ActiveKeyExchange for EcdhKeyExchange {
    fn pub_key(&self) -> &[u8] {
        match self {
            EcdhKeyExchange::X25519 { public_key, .. } => public_key,
            EcdhKeyExchange::P256 { public_key, .. } => public_key,
            EcdhKeyExchange::P384 { public_key, .. } => public_key,
        }
    }

    fn complete(self: Box<Self>, peer_pub: &[u8], out: &mut Buf) -> Result<(), CryptoError> {
        match *self {
            EcdhKeyExchange::X25519 { secret, .. } => {
                let peer_bytes: [u8; 32] = peer_pub
                    .try_into()
                    .map_err(|_| CryptoError::InvalidPublicKey(NamedGroup::X25519))?;
                let peer_key = x25519_dalek::PublicKey::from(peer_bytes);
                let shared_secret = secret.diffie_hellman(&peer_key);
                // RFC 7748 §6.1: check the shared secret is not zero (low-order point)
                if !shared_secret.was_contributory() {
                    return Err(CryptoError::InvalidPublicKey(NamedGroup::X25519));
                }
                out.clear();
                out.extend_from_slice(shared_secret.as_bytes());
                Ok(())
            }
            EcdhKeyExchange::P256 { secret, .. } => {
                let peer_key = P256PublicKey::from_sec1_bytes(peer_pub)
                    .map_err(|_| CryptoError::InvalidPublicKey(NamedGroup::Secp256r1))?;
                let shared_secret = secret.diffie_hellman(&peer_key);
                out.clear();
                out.extend_from_slice(shared_secret.raw_secret_bytes().as_slice());
                Ok(())
            }
            EcdhKeyExchange::P384 { secret, .. } => {
                let peer_key = P384PublicKey::from_sec1_bytes(peer_pub)
                    .map_err(|_| CryptoError::InvalidPublicKey(NamedGroup::Secp384r1))?;
                let shared_secret = secret.diffie_hellman(&peer_key);
                out.clear();
                out.extend_from_slice(shared_secret.raw_secret_bytes().as_slice());
                Ok(())
            }
        }
    }

    fn group(&self) -> NamedGroup {
        match self {
            EcdhKeyExchange::X25519 { .. } => NamedGroup::X25519,
            EcdhKeyExchange::P256 { .. } => NamedGroup::Secp256r1,
            EcdhKeyExchange::P384 { .. } => NamedGroup::Secp384r1,
        }
    }
}

/// X25519 key exchange group.
#[derive(Debug)]
struct X25519Kx;

impl SupportedKxGroup for X25519Kx {
    fn name(&self) -> NamedGroup {
        NamedGroup::X25519
    }

    fn start_exchange(&self, buf: Buf) -> Result<Box<dyn ActiveKeyExchange>, CryptoError> {
        Ok(Box::new(EcdhKeyExchange::new(NamedGroup::X25519, buf)?))
    }
}

/// P-256 (secp256r1) key exchange group.
#[derive(Debug)]
struct P256;

impl SupportedKxGroup for P256 {
    fn name(&self) -> NamedGroup {
        NamedGroup::Secp256r1
    }

    fn start_exchange(&self, buf: Buf) -> Result<Box<dyn ActiveKeyExchange>, CryptoError> {
        Ok(Box::new(EcdhKeyExchange::new(NamedGroup::Secp256r1, buf)?))
    }
}

/// P-384 (secp384r1) key exchange group.
#[derive(Debug)]
struct P384;

impl SupportedKxGroup for P384 {
    fn name(&self) -> NamedGroup {
        NamedGroup::Secp384r1
    }

    fn start_exchange(&self, buf: Buf) -> Result<Box<dyn ActiveKeyExchange>, CryptoError> {
        Ok(Box::new(EcdhKeyExchange::new(NamedGroup::Secp384r1, buf)?))
    }
}

/// Static instances of supported key exchange groups.
static KX_GROUP_X25519: X25519Kx = X25519Kx;
static KX_GROUP_P256: P256 = P256;
static KX_GROUP_P384: P384 = P384;

/// All supported key exchange groups.
pub(super) static ALL_KX_GROUPS: &[&dyn SupportedKxGroup] =
    &[&KX_GROUP_X25519, &KX_GROUP_P256, &KX_GROUP_P384];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x25519_non_contributory_peer_key_returns_invalid_public_key() {
        let exchange = X25519Kx
            .start_exchange(Buf::new())
            .expect("start key exchange");
        let mut out = Buf::new();

        assert_eq!(
            exchange.complete(&[0; 32], &mut out),
            Err(CryptoError::InvalidPublicKey(NamedGroup::X25519))
        );
    }
}
