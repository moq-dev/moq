use std::fmt;

use crate::vendor::dimpl::crypto::{ActiveKeyExchange, Buf, NamedGroup, SupportedKxGroup};
use crate::vendor::dimpl::{CryptoError, CryptoOperation};
use rind::agreement::{self, PrivateKey};

#[derive(Debug)]
struct Group {
	name: NamedGroup,
	algorithm: &'static agreement::Algorithm,
}

/// Key exchange groups, in order of preference.
pub(crate) static ALL: &[&dyn SupportedKxGroup] = &[
	&Group {
		name: NamedGroup::X25519,
		algorithm: &agreement::X25519,
	},
	&Group {
		name: NamedGroup::Secp256r1,
		algorithm: &agreement::ECDH_P256,
	},
	&Group {
		name: NamedGroup::Secp384r1,
		algorithm: &agreement::ECDH_P384,
	},
];

impl SupportedKxGroup for Group {
	fn name(&self) -> NamedGroup {
		self.name
	}

	fn start_exchange(&self, mut pub_key: Buf) -> Result<Box<dyn ActiveKeyExchange>, CryptoError> {
		let key = PrivateKey::generate(self.algorithm)
			.map_err(|_| CryptoError::OperationFailed(CryptoOperation::GenerateEphemeralKey))?;
		pub_key.clear();
		pub_key.extend_from_slice(key.public_key().as_ref());
		Ok(Box::new(Exchange {
			group: self.name,
			key,
			pub_key,
		}))
	}
}

struct Exchange {
	group: NamedGroup,
	key: PrivateKey,
	pub_key: Buf,
}

impl fmt::Debug for Exchange {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Exchange")
			.field("group", &self.group())
			.finish_non_exhaustive()
	}
}

impl ActiveKeyExchange for Exchange {
	fn pub_key(&self) -> &[u8] {
		&self.pub_key
	}

	fn complete(self: Box<Self>, peer_pub: &[u8], out: &mut Buf) -> Result<(), CryptoError> {
		// Covers a malformed or off-curve point, and X25519's all-zero output
		// from a low-order point (RFC 7748 section 6.1).
		let secret = self
			.key
			.agree(peer_pub)
			.map_err(|_| CryptoError::InvalidPublicKey(self.group))?;
		out.clear();
		out.extend_from_slice(secret.as_ref());
		Ok(())
	}

	fn group(&self) -> NamedGroup {
		self.group
	}
}
