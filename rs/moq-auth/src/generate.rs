use crate::{Algorithm, EllipticCurve, Error, Jwk, Key, KeyMaterial, RsaPublicKey};
use pkcs1::der::Decode as _;
use pkcs8::der::Decode as _;
use rind::signature::{EcdsaKeyPair, EcdsaSigningAlgorithm, Ed25519KeyPair, RsaKeyPair, RsaKeySize, sign};

/// Generate a key pair for the given algorithm, returning the private and public keys.
pub fn generate(algorithm: Algorithm, id: Option<crate::KeyId>) -> crate::Result<Key> {
	crate::crypto::init()?;

	let key = match algorithm {
		Algorithm::HS256 => generate_hmac_key::<32>(),
		Algorithm::HS384 => generate_hmac_key::<48>(),
		Algorithm::HS512 => generate_hmac_key::<64>(),
		Algorithm::RS256
		| Algorithm::RS384
		| Algorithm::RS512
		| Algorithm::PS256
		| Algorithm::PS384
		| Algorithm::PS512 => generate_rsa_key(),
		Algorithm::ES256 => generate_ec_key(&sign::ECDSA_P256_SHA256_FIXED, EllipticCurve::P256),
		Algorithm::ES384 => generate_ec_key(&sign::ECDSA_P384_SHA384_FIXED, EllipticCurve::P384),
		Algorithm::EdDSA => generate_ed25519_key(),
	};

	let mut jwk = Jwk::new(algorithm, key?);
	jwk.kid = id;
	jwk.try_into()
}

fn generate_hmac_key<const SIZE: usize>() -> crate::Result<KeyMaterial> {
	let mut key = [0u8; SIZE];
	rind::rand::fill(&mut key).map_err(Error::crypto)?;
	Ok(KeyMaterial::OCT { secret: key.to_vec() })
}

// rind exports an RSA private key only as PKCS#8, so each JWK field is read back out of it.
fn generate_rsa_key() -> crate::Result<KeyMaterial> {
	let pkcs8 = RsaKeyPair::generate(RsaKeySize::Rsa2048)
		.map_err(Error::crypto)?
		.to_pkcs8();
	let info = pkcs8::PrivateKeyInfoRef::from_der(pkcs8.as_ref()).map_err(Error::crypto)?;
	let key = pkcs1::RsaPrivateKey::from_der(info.private_key.as_bytes()).map_err(Error::crypto)?;

	Ok(KeyMaterial::RSA {
		public: RsaPublicKey {
			e: key.public_exponent.as_bytes().to_vec(),
			n: key.modulus.as_bytes().to_vec(),
		},
		private: Some(crate::RsaPrivateKey {
			d: key.private_exponent.as_bytes().to_vec(),
			p: key.prime1.as_bytes().to_vec(),
			q: key.prime2.as_bytes().to_vec(),
			dp: key.exponent1.as_bytes().to_vec(),
			dq: key.exponent2.as_bytes().to_vec(),
			qi: key.coefficient.as_bytes().to_vec(),
			oth: None,
		}),
	})
}

fn generate_ec_key(algorithm: &'static EcdsaSigningAlgorithm, curve: EllipticCurve) -> crate::Result<KeyMaterial> {
	let pair = EcdsaKeyPair::generate(algorithm).map_err(Error::crypto)?;

	// An X9.62 uncompressed point: a 0x04 tag, then x and y at equal width.
	let public = pair.public_key();
	let point = &public.as_ref()[1..];
	let (x, y) = point.split_at(point.len() / 2);

	Ok(KeyMaterial::EC {
		curve,
		x: x.to_vec(),
		y: y.to_vec(),
		d: Some(pair.to_scalar().as_ref().to_vec()),
	})
}

fn generate_ed25519_key() -> crate::Result<KeyMaterial> {
	let pair = Ed25519KeyPair::generate().map_err(Error::crypto)?;

	Ok(KeyMaterial::OKP {
		curve: EllipticCurve::Ed25519,
		x: pair.public_key().as_ref().to_vec(),
		d: Some(pair.to_seed().as_ref().to_vec()),
	})
}
