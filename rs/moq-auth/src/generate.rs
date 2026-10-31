use crate::error::KeyError;
use crate::{Algorithm, EllipticCurve, Jwk, Key, KeyMaterial, RsaPublicKey};
use aws_lc_rs::encoding::{AsBigEndian, AsDer, Pkcs8V1Der};
use aws_lc_rs::signature::KeyPair;
use p256::elliptic_curve::array::typenum::Unsigned;
use p256::elliptic_curve::point::PointCompression;
use p256::elliptic_curve::sec1::{FromSec1Point, ModulusSize, ToSec1Point};
use p256::elliptic_curve::{Curve, CurveArithmetic, SecretKey};
use pkcs1::der::Decode;

/// Generate a key pair for the given algorithm, returning the private and public keys.
pub fn generate(algorithm: Algorithm, id: Option<crate::KeyId>) -> crate::Result<Key> {
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
		Algorithm::ES256 => generate_ec_key::<p256::NistP256>(EllipticCurve::P256),
		Algorithm::ES384 => generate_ec_key::<p384::NistP384>(EllipticCurve::P384),
		Algorithm::EdDSA => generate_ed25519_key(),
	};

	let mut jwk = Jwk::new(algorithm, key?);
	jwk.kid = id;
	jwk.try_into()
}

fn generate_hmac_key<const SIZE: usize>() -> crate::Result<KeyMaterial> {
	let mut key = [0u8; SIZE];
	aws_lc_rs::rand::fill(&mut key)?;
	Ok(KeyMaterial::OCT { secret: key.to_vec() })
}

fn generate_rsa_key() -> crate::Result<KeyMaterial> {
	let key = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048)?;
	let document: Pkcs8V1Der<'_> = key.as_der()?;
	let info =
		pkcs8::PrivateKeyInfoRef::try_from(document.as_ref()).map_err(|e| crate::Error::Crypto(e.to_string()))?;
	let key =
		pkcs1::RsaPrivateKey::from_der(info.private_key.as_bytes()).map_err(|e| crate::Error::Crypto(e.to_string()))?;

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

fn generate_ec_key<C>(curve: EllipticCurve) -> crate::Result<KeyMaterial>
where
	C: Curve + CurveArithmetic + PointCompression,
	C::AffinePoint: ToSec1Point<C> + FromSec1Point<C>,
	C::FieldBytesSize: ModulusSize,
{
	let mut bytes = vec![0u8; C::FieldBytesSize::to_usize()];
	let secret = loop {
		aws_lc_rs::rand::fill(&mut bytes)?;

		if let Ok(secret) = SecretKey::<C>::from_slice(&bytes) {
			break secret;
		}
	};

	let point = secret.public_key().to_sec1_point(false);

	let x = point.x().ok_or(KeyError::MissingEcX)?.to_vec();
	let y = point.y().ok_or(KeyError::MissingEcY)?.to_vec();
	let d = secret.to_bytes().to_vec();

	Ok(KeyMaterial::EC {
		curve,
		x,
		y,
		d: Some(d),
	})
}

fn generate_ed25519_key() -> crate::Result<KeyMaterial> {
	let key_pair = aws_lc_rs::signature::Ed25519KeyPair::generate()?;

	let public_key = key_pair.public_key().as_ref().to_vec();
	let seed = key_pair.seed()?.as_be_bytes()?.as_ref().to_vec();

	Ok(KeyMaterial::OKP {
		curve: EllipticCurve::Ed25519,
		x: public_key,
		d: Some(seed),
	})
}
