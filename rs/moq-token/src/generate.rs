use crate::{Algorithm, EllipticCurve, Key, KeyOperation, KeyType, RsaPublicKey};
use anyhow::Context;
use aws_lc_rs::encoding::AsBigEndian;
use aws_lc_rs::signature::KeyPair;
use elliptic_curve::generic_array::typenum::Unsigned;
use elliptic_curve::point::PointCompression;
use elliptic_curve::sec1::{FromEncodedPoint, ModulusSize, ToEncodedPoint};
use elliptic_curve::{Curve, CurveArithmetic, SecretKey};
use rsa::traits::{PrivateKeyParts, PublicKeyParts};

/// Generate a key pair for the given algorithm, returning the private and public keys.
pub fn generate(algorithm: Algorithm, id: Option<String>) -> anyhow::Result<Key> {
	let key = match algorithm {
		Algorithm::HS256 => generate_hmac_key::<32>(),
		Algorithm::HS384 => generate_hmac_key::<48>(),
		Algorithm::HS512 => generate_hmac_key::<64>(),
		Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => generate_rsa_key(2048),
		Algorithm::PS256 | Algorithm::PS384 | Algorithm::PS512 => generate_rsa_key(2048),
		Algorithm::ES256 => generate_ec_key::<p256::NistP256>(EllipticCurve::P256),
		Algorithm::ES384 => generate_ec_key::<p384::NistP384>(EllipticCurve::P384),
		Algorithm::EdDSA => generate_ed25519_key(),
	};

	Ok(Key {
		kid: id,
		operations: [KeyOperation::Sign, KeyOperation::Verify].into(),
		algorithm,
		key: key?,
		decode: Default::default(),
		encode: Default::default(),
	})
}

fn generate_hmac_key<const SIZE: usize>() -> anyhow::Result<KeyType> {
	let mut key = [0u8; SIZE];
	aws_lc_rs::rand::fill(&mut key)?;
	Ok(KeyType::OCT { secret: key.to_vec() })
}

struct AwsRng;

impl rsa::rand_core::RngCore for AwsRng {
	fn next_u32(&mut self) -> u32 {
		let mut buf = [0u8; 4];
		self.fill_bytes(&mut buf);
		u32::from_le_bytes(buf)
	}

	fn next_u64(&mut self) -> u64 {
		let mut buf = [0u8; 8];
		self.fill_bytes(&mut buf);
		u64::from_le_bytes(buf)
	}

	fn fill_bytes(&mut self, dest: &mut [u8]) {
		aws_lc_rs::rand::fill(dest).unwrap();
	}

	fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rsa::rand_core::Error> {
		aws_lc_rs::rand::fill(dest).map_err(|_| rsa::rand_core::Error::new("aws-lc-rs failed"))
	}
}

impl rsa::rand_core::CryptoRng for AwsRng {}

fn generate_rsa_key(size: usize) -> anyhow::Result<KeyType> {
	let mut rng = AwsRng;
	let key = rsa::RsaPrivateKey::new(&mut rng, size);

	match key {
		Ok(key) => Ok(KeyType::RSA {
			public: RsaPublicKey {
				e: key.e().to_bytes_be(),
				n: key.n().to_bytes_be(),
			},
			private: Some(crate::RsaPrivateKey {
				d: key.d().to_bytes_be(),
				p: key.primes()[0].to_bytes_be(),
				q: key.primes()[1].to_bytes_be(),
				dp: key.dp().expect("no dp specified in key").to_bytes_be(),
				dq: key.dq().expect("no dq specified in key").to_bytes_be(),
				qi: key.qinv().expect("no qinv specified in key").to_bytes_be().1,
				oth: None, // TODO https://datatracker.ietf.org/doc/html/rfc7518#section-6.3.2.7
			}),
		}),
		Err(err) => Err(anyhow::anyhow!("Failed to generate RSA key: {}", err)),
	}
}

fn generate_ec_key<C>(curve: EllipticCurve) -> anyhow::Result<KeyType>
where
	C: Curve + CurveArithmetic + PointCompression,
	C::AffinePoint: ToEncodedPoint<C> + FromEncodedPoint<C>,
	C::FieldBytesSize: ModulusSize,
{
	let mut bytes = vec![0u8; C::FieldBytesSize::to_usize()];
	let secret = loop {
		aws_lc_rs::rand::fill(&mut bytes)?;

		if let Ok(secret) = SecretKey::<C>::from_slice(&bytes) {
			break secret;
		}
	};

	let point = secret.public_key().to_encoded_point(false);

	let x = point.x().context("Missing x() point in EC key")?.to_vec();
	let y = point.y().context("Missing y() point in EC key")?.to_vec();
	let d = secret.to_bytes().to_vec();

	Ok(KeyType::EC {
		curve,
		x,
		y,
		d: Some(d),
	})
}

fn generate_ed25519_key() -> anyhow::Result<KeyType> {
	let key_pair = aws_lc_rs::signature::Ed25519KeyPair::generate()?;

	let public_key = key_pair.public_key().as_ref().to_vec();
	let seed = key_pair.seed()?.as_be_bytes()?.as_ref().to_vec();

	Ok(KeyType::OKP {
		curve: EllipticCurve::Ed25519,
		x: public_key,
		d: Some(seed),
	})
}
