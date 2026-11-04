//! An endpoint's persistent Ed25519 secret key.

use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;

use rind::signature::Ed25519KeyPair;
use zeroize::Zeroizing;

use crate::sign::key::{KEY_LEN, PublicKey, ZBASE32, decode};
use crate::sign::scope::SIGNATURE_LEN;

/// The PKCS#8 v1 (RFC 8410) encoding of an Ed25519 key, up to its 32-byte seed. rind loads a seed
/// alone only this way, and the key file stores the seed alone.
const PKCS8_SEED_PREFIX: [u8; 16] = [
	0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

struct Inner {
	pair: Ed25519KeyPair,
	public: PublicKey,
}

/// An endpoint's Ed25519 secret key: cheap to clone, redacted from `Debug`.
#[derive(Clone)]
pub struct SecretKey(Arc<Inner>);

impl SecretKey {
	/// Generate a fresh key from the system's secure random source.
	pub fn generate() -> Self {
		Self::new(Ed25519KeyPair::generate().expect("system randomness"))
	}

	pub(crate) fn from_seed(seed: &[u8; KEY_LEN]) -> Self {
		let mut der = Zeroizing::new([0u8; PKCS8_SEED_PREFIX.len() + KEY_LEN]);
		der[..PKCS8_SEED_PREFIX.len()].copy_from_slice(&PKCS8_SEED_PREFIX);
		der[PKCS8_SEED_PREFIX.len()..].copy_from_slice(seed);
		Self::new(Ed25519KeyPair::from_pkcs8(der.as_ref()).expect("a PKCS#8 v1 Ed25519 key"))
	}

	fn new(pair: Ed25519KeyPair) -> Self {
		let public = PublicKey::from_bytes(
			pair.public_key()
				.as_ref()
				.try_into()
				.expect("a 32-byte Ed25519 public key"),
		);
		Self(Arc::new(Inner { pair, public }))
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
		ZBASE32.encode_append(key.0.pair.to_seed().as_ref(), &mut text);
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
		let seed = Zeroizing::new(decode(encoded).map_err(|_| {
			io::Error::new(
				io::ErrorKind::InvalidData,
				format!("key file {} is not 52 z-base-32 characters", path.display()),
			)
		})?);
		Ok(Self::from_seed(&seed))
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
