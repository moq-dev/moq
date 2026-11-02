//! Endpoint keys: a persistent Ed25519 secret key and the public key that names its endpoint.

use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;
use std::sync::Arc;

use aws_lc_rs::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use zeroize::Zeroizing;

use crate::scope::SIGNATURE_LEN;

/// Ed25519 key length in bytes, for both the secret seed and the public key.
const KEY_LEN: usize = 32;

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
/// zero trailing bits), so one key has exactly one ID to name a path by.
///
/// [z-base-32]: https://philzimmermann.com/docs/human-oriented-base-32-encoding.txt
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey([u8; KEY_LEN]);

impl PublicKey {
	#[cfg(test)]
	pub(crate) fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
		Self(bytes)
	}

	/// Whether `signature` is this key's Ed25519 signature over `message`.
	pub(crate) fn verify(&self, message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> bool {
		UnparsedPublicKey::new(&ED25519, &self.0)
			.verify(message, signature)
			.is_ok()
	}
}

/// The text is not the 52-character z-base-32 of a 32-byte key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid key: expected 52 z-base-32 characters")]
#[non_exhaustive]
pub struct InvalidKey;

impl FromStr for PublicKey {
	type Err = InvalidKey;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		Ok(Self(decode(text)?))
	}
}

impl fmt::Display for PublicKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&ZBASE32.encode(&self.0))
	}
}

impl fmt::Debug for PublicKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("PublicKey").field(&format_args!("{self}")).finish()
	}
}

/// Decode exactly 52 z-base-32 characters. The alphabet is lowercase-only and nonzero trailing
/// bits are refused, so a key has one text form.
fn decode(text: &str) -> Result<[u8; KEY_LEN], InvalidKey> {
	let mut out = [0u8; KEY_LEN];
	match ZBASE32.decode_len(text.len()) {
		Ok(KEY_LEN) => ZBASE32.decode_mut(text.as_bytes(), &mut out).map_err(|_| InvalidKey)?,
		_ => return Err(InvalidKey),
	};
	Ok(out)
}

struct Inner {
	seed: Zeroizing<[u8; KEY_LEN]>,
	pair: Ed25519KeyPair,
	public: PublicKey,
}

/// An endpoint's Ed25519 secret key: cheap to clone, redacted from `Debug`.
#[derive(Clone)]
pub struct SecretKey(Arc<Inner>);

impl SecretKey {
	/// Generate a fresh key from the system's secure random source.
	pub fn generate() -> Self {
		let mut seed = Zeroizing::new([0u8; KEY_LEN]);
		aws_lc_rs::rand::fill(seed.as_mut()).expect("system randomness");
		Self::from_seed(seed)
	}

	pub(crate) fn from_seed(seed: Zeroizing<[u8; KEY_LEN]>) -> Self {
		let pair = Ed25519KeyPair::from_seed_unchecked(seed.as_ref()).expect("a 32-byte Ed25519 seed");
		let public = PublicKey(
			pair.public_key()
				.as_ref()
				.try_into()
				.expect("a 32-byte Ed25519 public key"),
		);
		Self(Arc::new(Inner { seed, pair, public }))
	}

	/// The public key, which is this endpoint's ID.
	pub fn public(&self) -> PublicKey {
		self.0.public
	}

	/// Load the key stored at `path`, or generate one and store it there if the file is missing.
	///
	/// The file holds the seed as 52 z-base-32 characters and a newline, created with mode
	/// `0600`. A key is never replaced implicitly: an existing file that does not parse, or that
	/// other users can read, is an error rather than a reason to generate a new identity.
	///
	/// # Errors
	///
	/// [`io::ErrorKind::InvalidData`] for a malformed or exposed file, or the I/O error reading or
	/// creating it.
	pub fn load_or_generate(path: impl AsRef<std::path::Path>) -> io::Result<Self> {
		let path = path.as_ref();
		match Self::load(path) {
			Err(err) if err.kind() == io::ErrorKind::NotFound => {}
			other => return other,
		}

		let key = Self::generate();
		let mut options = std::fs::OpenOptions::new();
		options.write(true).create_new(true);
		#[cfg(unix)]
		std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
		let mut file = match options.open(path) {
			Ok(file) => file,
			// Another process created it first; its key wins.
			Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Self::load(path),
			Err(err) => return Err(err),
		};
		// Sized up front, so appending the newline never leaves an unzeroized copy behind.
		let mut text = Zeroizing::new(String::with_capacity(ZBASE32.encode_len(KEY_LEN) + 1));
		ZBASE32.encode_append(key.0.seed.as_ref(), &mut text);
		text.push('\n');
		file.write_all(text.as_bytes())?;
		file.sync_all()?;
		Ok(key)
	}

	fn load(path: &std::path::Path) -> io::Result<Self> {
		let file = std::fs::File::open(path)?;
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			if file.metadata()?.permissions().mode() & 0o077 != 0 {
				return Err(io::Error::new(
					io::ErrorKind::InvalidData,
					format!("key file {} is accessible by other users", path.display()),
				));
			}
		}
		let mut text = Zeroizing::new(String::new());
		io::Read::read_to_string(&mut &file, &mut text)?;
		let encoded = text.strip_suffix('\n').unwrap_or(&text);
		let seed = decode(encoded).map_err(|_| {
			io::Error::new(
				io::ErrorKind::InvalidData,
				format!("key file {} is not 52 z-base-32 characters", path.display()),
			)
		})?;
		Ok(Self::from_seed(Zeroizing::new(seed)))
	}

	/// Sign `message` with Ed25519.
	pub(crate) fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
		self.0
			.pair
			.sign(message)
			.as_ref()
			.try_into()
			.expect("a 64-byte Ed25519 signature")
	}
}

impl fmt::Debug for SecretKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("SecretKey")
			.field("public", &self.0.public)
			.field("seed", &"<redacted>")
			.finish()
	}
}
