//! Mesh enrollment: how a device with an endpoint key joins a mesh.
//!
//! A device proves it holds its Ed25519 endpoint key by signing a fresh challenge from
//! the coordinator, and presents an enrollment key the mesh owner created, so enrollment
//! needs no human in the loop. The coordinator answers with an
//! [`Enrollment`]: the relays to dial and a JWT granting publish on `mesh/<id>/**` and
//! subscribe on `mesh/**`. The device refreshes it before it expires by proving its key
//! again, so revoking a device makes its next refresh fail.
//!
//! These types are the JSON contract. [`Coordinator`] (feature `serve`) answers it behind
//! `moq auth serve`; [`Client`] (feature `client`) drives it from a device. A hosted
//! service implements the same contract with its own accounts.
//!
//! | Request | Body | Reply |
//! |---|---|---|
//! | `POST {base}/challenge` | none | [`Challenge`] |
//! | `POST {base}/enroll` | [`EnrollRequest`] | [`Enrollment`] |
//! | `POST {base}/refresh` | [`Proof`] | [`Enrollment`] |
//!
//! Every refusal is a 4xx with an [`ErrorReply`].

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[cfg(feature = "client")]
mod client;
#[cfg(all(test, feature = "client", feature = "serve"))]
mod contract_tests;
#[cfg(feature = "serve")]
mod coordinator;

#[cfg(feature = "client")]
pub use client::*;
#[cfg(feature = "serve")]
pub use coordinator::*;

/// The context string every [`Proof`] signature starts with, so a signature made for
/// enrollment can never be replayed as anything else the endpoint key signs.
pub const PROOF_CONTEXT: &[u8] = b"moq-mesh-enroll-00";

/// The length of a challenge in bytes.
pub const CHALLENGE_LEN: usize = 32;

const KEY_LEN: usize = 32;
const SIGNATURE_LEN: usize = 64;

/// z-base-32: most significant bit first, no padding, trailing bits must be zero.
static ZBASE32: std::sync::LazyLock<data_encoding::Encoding> = std::sync::LazyLock::new(|| {
	let mut spec = data_encoding::Specification::new();
	spec.symbols.push_str("ybndrfg8ejkmcpqxot1uwisza345h769");
	spec.encoding().expect("a valid z-base-32 specification")
});

/// An endpoint's Ed25519 public key, which is also its endpoint ID.
///
/// Its text form is the 52-character [z-base-32] of the key, the form pkarr and iroh's DNS
/// discovery use, which is also a valid DNS label. Parsing accepts only that form (lowercase,
/// zero trailing bits), so one key names exactly one `mesh/<id>` path.
///
/// [z-base-32]: https://philzimmermann.com/docs/human-oriented-base-32-encoding.txt
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EndpointId([u8; KEY_LEN]);

impl EndpointId {
	/// The ID of the raw 32-byte Ed25519 public key.
	pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
		Self(bytes)
	}

	/// The raw 32-byte Ed25519 public key.
	pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
		&self.0
	}

	/// Whether `signature` is this key's Ed25519 signature over `message`.
	pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
		use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};
		UnparsedPublicKey::new(&ED25519, &self.0)
			.verify(message, signature)
			.is_ok()
	}

	/// The broadcasts this endpoint may publish: `mesh/<id>/**`.
	pub fn publish(&self) -> crate::Pattern {
		crate::Pattern::subtree(&format!("mesh/{self}")).expect("a z-base-32 segment is a valid path")
	}
}

/// The text is not the 52-character z-base-32 of a 32-byte key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid endpoint ID: expected 52 z-base-32 characters")]
#[non_exhaustive]
pub struct InvalidEndpoint;

impl FromStr for EndpointId {
	type Err = InvalidEndpoint;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		// The alphabet is lowercase-only and nonzero trailing bits are refused, so a key has one text form.
		let mut out = [0u8; KEY_LEN];
		match ZBASE32.decode_len(text.len()) {
			Ok(KEY_LEN) => ZBASE32
				.decode_mut(text.as_bytes(), &mut out)
				.map_err(|_| InvalidEndpoint)?,
			_ => return Err(InvalidEndpoint),
		};
		Ok(Self(out))
	}
}

impl fmt::Display for EndpointId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&ZBASE32.encode(&self.0))
	}
}

impl fmt::Debug for EndpointId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("EndpointId").field(&format_args!("{self}")).finish()
	}
}

impl Serialize for EndpointId {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_str(self)
	}
}

impl<'de> Deserialize<'de> for EndpointId {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
		text.parse().map_err(serde::de::Error::custom)
	}
}

/// The message a [`Proof`] signs: [`PROOF_CONTEXT`], a zero byte, the coordinator's base
/// URL as the device addressed it, a zero byte, then the raw challenge.
///
/// The URL binds the proof to one coordinator, so a coordinator the device trusts cannot
/// relay another's challenge and enroll the device's key there.
pub fn proof_message(base: &url::Url, challenge: &[u8]) -> Vec<u8> {
	let base = audience(base);
	let mut message = Vec::with_capacity(PROOF_CONTEXT.len() + base.len() + challenge.len() + 2);
	message.extend_from_slice(PROOF_CONTEXT);
	message.push(0);
	message.extend_from_slice(base.as_bytes());
	message.push(0);
	message.extend_from_slice(challenge);
	message
}

/// The base URL without a trailing slash, the form both sides sign over.
fn audience(base: &url::Url) -> &str {
	base.as_str().trim_end_matches('/')
}

/// A fresh, single-use challenge from `POST {base}/challenge`.
#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
	/// [`CHALLENGE_LEN`] bytes, opaque to the device, base64url without padding.
	#[serde_as(as = "serde_with::base64::Base64<serde_with::base64::UrlSafe, serde_with::formats::Unpadded>")]
	pub challenge: Vec<u8>,
	/// When the coordinator stops accepting a proof over it, unix seconds.
	#[serde_as(as = "moq_clock::serde::UnixSeconds")]
	pub expires: moq_clock::Wall,
}

/// Proof that the caller holds `endpoint`'s secret key: its signature over
/// [`proof_message`] for a challenge the coordinator issued and has not yet seen used.
#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
	/// The endpoint ID, which is the key that signed.
	pub endpoint: EndpointId,
	/// The challenge, as issued.
	#[serde_as(as = "serde_with::base64::Base64<serde_with::base64::UrlSafe, serde_with::formats::Unpadded>")]
	pub challenge: Vec<u8>,
	/// The Ed25519 signature over [`proof_message`].
	#[serde_as(as = "serde_with::base64::Base64<serde_with::base64::UrlSafe, serde_with::formats::Unpadded>")]
	pub signature: Vec<u8>,
}

impl Proof {
	/// A proof for `challenge` at the coordinator `base`, signed by `signer`.
	pub fn sign(signer: &dyn Signer, base: &url::Url, challenge: Vec<u8>) -> Self {
		let signature = signer.sign(&proof_message(base, &challenge)).to_vec();
		Self {
			endpoint: signer.endpoint(),
			challenge,
			signature,
		}
	}

	/// Whether the signature verifies for the coordinator `base`. The challenge's
	/// freshness is the coordinator's to check.
	pub fn verify(&self, base: &url::Url) -> bool {
		self.signature.len() == SIGNATURE_LEN
			&& self
				.endpoint
				.verify(&proof_message(base, &self.challenge), &self.signature)
	}
}

/// An endpoint's secret key, held by whatever stores it.
///
/// Implemented for [`aws_lc_rs::signature::Ed25519KeyPair`]; a daemon implements it for
/// its own key type.
pub trait Signer: Send + Sync {
	/// The endpoint ID: this key's public half.
	fn endpoint(&self) -> EndpointId;
	/// The Ed25519 signature over `message`.
	fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN];
}

impl Signer for aws_lc_rs::signature::Ed25519KeyPair {
	fn endpoint(&self) -> EndpointId {
		use aws_lc_rs::signature::KeyPair;
		EndpointId(
			self.public_key()
				.as_ref()
				.try_into()
				.expect("a 32-byte Ed25519 public key"),
		)
	}

	fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
		aws_lc_rs::signature::Ed25519KeyPair::sign(self, message)
			.as_ref()
			.try_into()
			.expect("a 64-byte Ed25519 signature")
	}
}

/// `POST {base}/enroll`: join with a pre-authorized enrollment key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollRequest {
	/// Proof of the endpoint key.
	#[serde(flatten)]
	pub proof: Proof,
	/// The enrollment key the mesh owner created.
	pub key: String,
	/// The device's requested name, usually its hostname. The coordinator may change it to
	/// keep names unique and DNS-safe; the [`Enrollment`] carries the one it assigned.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
}

/// What a device needs to join: the relays to dial and the token to present.
#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrollment {
	/// A JWT granting publish on `mesh/<id>/**` and subscribe on `mesh/**`, rooted at `/`.
	pub token: String,
	/// When the token expires, unix seconds. Refresh before then.
	#[serde_as(as = "moq_clock::serde::UnixSeconds")]
	pub expires: moq_clock::Wall,
	/// The relays to dial, presenting the token.
	pub relays: Vec<url::Url>,
	/// The name the coordinator assigned this device.
	pub name: String,
	/// Every enrolled member of the mesh, this device included.
	pub members: Vec<Member>,
}

/// One enrolled device, as listed in an [`Enrollment`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
	/// Its endpoint ID.
	pub endpoint: EndpointId,
	/// Its unique, DNS-safe name.
	pub name: String,
	/// The tags of the enrollment key it joined with.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<String>,
}

/// Why the coordinator refused, in the shape of an OAuth error response (RFC 6749 §5.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{error}{}", description.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
#[non_exhaustive]
pub struct ErrorReply {
	/// The machine-readable reason.
	pub error: ErrorCode,
	/// A human-readable detail.
	#[serde(rename = "error_description", default, skip_serializing_if = "Option::is_none")]
	pub description: Option<String>,
}

impl ErrorReply {
	/// A refusal for `error` with a human-readable detail.
	pub fn new(error: ErrorCode, description: impl Into<String>) -> Self {
		Self {
			error,
			description: Some(description.into()),
		}
	}
}

/// The reasons a coordinator refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
	/// The body did not parse.
	InvalidRequest,
	/// The challenge is unknown, used, or expired; fetch a new one.
	InvalidChallenge,
	/// The signature does not verify against the endpoint ID.
	InvalidSignature,
	/// The enrollment key is unknown, used, revoked, or expired.
	InvalidGrant,
	/// The coordinator is busy; retry shortly.
	SlowDown,
	/// The device is revoked or not enrolled.
	AccessDenied,
	/// The coordinator failed on its side; retry later.
	ServerError,
	/// A reason this version does not know.
	#[serde(other)]
	Unknown,
}

impl fmt::Display for ErrorCode {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let value = serde_json::to_value(self).expect("an enum serializes");
		f.write_str(value.as_str().unwrap_or("unknown"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn signer() -> aws_lc_rs::signature::Ed25519KeyPair {
		aws_lc_rs::signature::Ed25519KeyPair::generate().unwrap()
	}

	#[test]
	fn endpoint_ids_have_one_text_form() {
		let id = signer().endpoint();
		let text = id.to_string();
		assert_eq!(text.len(), 52);
		assert_eq!(text.parse::<EndpointId>().unwrap(), id);
		assert!(text.to_uppercase().parse::<EndpointId>().is_err());
		assert!(text[..51].parse::<EndpointId>().is_err());
		assert!(format!("{text}y").parse::<EndpointId>().is_err());
		assert!(
			data_encoding::HEXLOWER
				.encode(id.as_bytes())
				.parse::<EndpointId>()
				.is_err()
		);
		assert_eq!(id.publish().as_str(), format!("mesh/{text}/**"));
	}

	#[test]
	fn endpoint_ids_are_z_base_32() {
		assert_eq!(EndpointId::from_bytes([0; KEY_LEN]).to_string(), "y".repeat(52));
		let ones = EndpointId::from_bytes([0xff; KEY_LEN]);
		assert_eq!(ones.to_string(), format!("{}o", "9".repeat(51)));
		// The last character carries 4 padding bits, which must be zero.
		assert!(format!("{}9", "9".repeat(51)).parse::<EndpointId>().is_err());
	}

	#[test]
	fn a_proof_is_bound_to_its_coordinator() {
		let key = signer();
		let base: url::Url = "https://a.example/mesh".parse().unwrap();
		let proof = Proof::sign(&key, &base, vec![7; CHALLENGE_LEN]);
		assert!(proof.verify(&base));
		assert!(proof.verify(&"https://a.example/mesh/".parse().unwrap()));
		assert!(!proof.verify(&"https://b.example/mesh".parse().unwrap()));

		let mut forged = proof.clone();
		forged.endpoint = signer().endpoint();
		assert!(!forged.verify(&base));
	}

	#[test]
	fn error_replies_read_unknown_codes() {
		let reply: ErrorReply = serde_json::from_str(r#"{"error":"access_denied"}"#).unwrap();
		assert_eq!(reply.error, ErrorCode::AccessDenied);
		let reply: ErrorReply = serde_json::from_str(r#"{"error":"new_thing","error_description":"x"}"#).unwrap();
		assert_eq!(reply.error, ErrorCode::Unknown);
		assert_eq!(ErrorCode::SlowDown.to_string(), "slow_down");
	}
}
