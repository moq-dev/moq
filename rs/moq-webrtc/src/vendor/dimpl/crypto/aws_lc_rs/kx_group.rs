//! Key exchange group implementations using aws-lc-rs.

use aws_lc_rs::agreement::{ECDH_P256, ECDH_P384, UnparsedPublicKey, X25519};
use aws_lc_rs::agreement::{EphemeralPrivateKey, agree_ephemeral};

use super::super::{ActiveKeyExchange, SupportedKxGroup};
use crate::buffer::Buf;
use crate::types::NamedGroup;
use crate::{CryptoError, CryptoOperation};

/// ECDHE key exchange implementation.
struct EcdhKeyExchange {
    group: NamedGroup,
    private_key: EphemeralPrivateKey,
    public_key: Buf,
}

impl std::fmt::Debug for EcdhKeyExchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EcdhKeyExchange")
            .field("group", &self.group)
            .field("public_key_len", &self.public_key.len())
            .finish_non_exhaustive()
    }
}

impl EcdhKeyExchange {
    fn new(group: NamedGroup, mut buf: Buf) -> Result<Self, CryptoError> {
        let algorithm = match group {
            NamedGroup::X25519 => &X25519,
            NamedGroup::Secp256r1 => &ECDH_P256,
            NamedGroup::Secp384r1 => &ECDH_P384,
            _ => return Err(CryptoError::UnsupportedKeyExchangeGroup(group)),
        };

        let rng = aws_lc_rs::rand::SystemRandom::new();
        let private_key = EphemeralPrivateKey::generate(algorithm, &rng)
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::GenerateEphemeralKey))?;

        let pk = private_key
            .compute_public_key()
            .map_err(|_| CryptoError::OperationFailed(CryptoOperation::ComputePublicKey))?;

        buf.clear();
        buf.extend_from_slice(pk.as_ref());

        Ok(EcdhKeyExchange {
            group,
            private_key,
            public_key: buf,
        })
    }

    fn algorithm(&self) -> &'static aws_lc_rs::agreement::Algorithm {
        match self.group {
            NamedGroup::X25519 => &X25519,
            NamedGroup::Secp256r1 => &ECDH_P256,
            NamedGroup::Secp384r1 => &ECDH_P384,
            _ => unreachable!("Unsupported group"),
        }
    }
}

impl ActiveKeyExchange for EcdhKeyExchange {
    fn pub_key(&self) -> &[u8] {
        &self.public_key
    }

    fn complete(self: Box<Self>, peer_pub: &[u8], out: &mut Buf) -> Result<(), CryptoError> {
        let algorithm = self.algorithm();
        let peer_key = UnparsedPublicKey::new(algorithm, peer_pub);

        // RFC 7748 §6.1: agree_ephemeral rejects non-contributory shared secrets internally
        agree_ephemeral(
            self.private_key,
            peer_key,
            "ECDH agreement failed",
            |secret| {
                out.clear();
                out.extend_from_slice(secret);
                Ok(())
            },
        )
        .map_err(|_| CryptoError::InvalidPublicKey(self.group))
    }

    fn group(&self) -> NamedGroup {
        self.group
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
