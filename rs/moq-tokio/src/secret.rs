//! A 32-byte secret an operator configures as a file, such as `cluster.lan.secret`
//! or `listen.reset.secret`.

use std::fmt;
use std::path::{Path, PathBuf};

/// Load a persisted secret, or exclusively create its private file.
#[cfg(any(feature = "iroh", test))]
pub(crate) async fn load_or_create(path: &Path, generated: &str) -> std::io::Result<String> {
	use std::io;
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	let mut options = std::fs::OpenOptions::new();
	options.write(true).create_new(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		options.mode(0o600);
	}
	match tokio::fs::OpenOptions::from(options).open(path).await {
		Ok(mut file) => {
			file.write_all(generated.as_bytes()).await?;
			file.flush().await?;
			return Ok(generated.to_owned());
		}
		Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
		Err(err) => return Err(err),
	}

	let mut options = std::fs::OpenOptions::new();
	options.read(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		// Validate the opened inode, without following a symlink or blocking on a FIFO.
		options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
	}
	#[cfg(windows)]
	{
		use std::os::windows::fs::OpenOptionsExt;
		// FILE_FLAG_OPEN_REPARSE_POINT opens the link itself for validation.
		options.custom_flags(0x00200000);
	}
	let mut file = tokio::fs::OpenOptions::from(options).open(path).await?;
	let metadata = file.metadata().await?;
	if !metadata.is_file() {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			"secret must be a regular file",
		));
	}
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		if metadata.permissions().mode() & 0o044 != 0 {
			return Err(io::Error::new(
				io::ErrorKind::PermissionDenied,
				"secret must not be readable by group or others; use chmod 600",
			));
		}
	}
	let mut contents = String::new();
	file.read_to_string(&mut contents).await?;
	Ok(contents)
}

/// A secret file could not be read or decoded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The file does not hold exactly 64 hexadecimal characters.
	#[error("secret file {0} must hold exactly 64 hexadecimal characters (`openssl rand -hex 32`)")]
	Invalid(PathBuf),

	/// The file could not be read.
	#[error("failed to read secret file {0}")]
	Read(PathBuf, #[source] std::io::Error),
}

/// A random 32-byte secret.
///
/// Anyone watching what it keys can brute-force a weak one offline, so generate
/// it (`openssl rand -hex 32`) rather than choosing one. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; 32]);

impl Secret {
	/// Read a secret from a file holding 64 hexadecimal characters, optionally
	/// followed by one newline, as `openssl rand -hex 32` writes it.
	pub fn read(path: impl AsRef<Path>) -> Result<Self, Error> {
		let path = path.as_ref();
		let contents = std::fs::read(path).map_err(|err| Error::Read(path.to_path_buf(), err))?;
		let hex = contents.strip_suffix(b"\n").unwrap_or(&contents);
		let mut secret = [0; 32];
		hex::decode_to_slice(hex, &mut secret).map_err(|_| Error::Invalid(path.to_path_buf()))?;
		Ok(Self(secret))
	}

	/// The secret's 32 bytes.
	pub fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}
}

impl From<[u8; 32]> for Secret {
	fn from(secret: [u8; 32]) -> Self {
		Self(secret)
	}
}

impl fmt::Debug for Secret {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str("Secret([redacted])")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

	#[tokio::test]
	async fn persisted_secret_is_private_and_reused() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("secret.key");
		assert_eq!(load_or_create(&path, KEY).await.unwrap(), KEY);
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			let metadata = std::fs::metadata(&path).unwrap();
			assert_eq!(metadata.permissions().mode() & 0o177, 0);
			std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
		}
		assert_eq!(load_or_create(&path, "replacement").await.unwrap(), KEY);
		assert_eq!(std::fs::read_to_string(&path).unwrap(), KEY);
	}

	#[cfg(unix)]
	#[tokio::test]
	async fn persisted_secret_refuses_shared_read_permissions() {
		use std::os::unix::fs::PermissionsExt;

		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("secret.key");
		std::fs::write(&path, KEY).unwrap();
		for mode in [0o640, 0o604, 0o644] {
			std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
			let err = load_or_create(&path, "replacement").await.unwrap_err();
			assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
			assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, mode);
			assert_eq!(std::fs::read_to_string(&path).unwrap(), KEY);
		}
	}

	#[cfg(unix)]
	#[tokio::test]
	async fn persisted_secret_refuses_symlinks_without_touching_the_target() {
		use std::os::unix::fs::symlink;

		let dir = tempfile::tempdir().unwrap();
		let target = dir.path().join("target.key");
		let link = dir.path().join("secret.key");
		symlink(&target, &link).unwrap();
		assert!(load_or_create(&link, KEY).await.is_err());
		assert!(!target.exists());
		std::fs::write(&target, KEY).unwrap();
		assert!(load_or_create(&link, "replacement").await.is_err());
		assert_eq!(std::fs::read_to_string(&target).unwrap(), KEY);
	}

	#[tokio::test]
	async fn persisted_secret_refuses_nonregular_files() {
		let dir = tempfile::tempdir().unwrap();
		assert!(load_or_create(dir.path(), KEY).await.is_err());
		#[cfg(unix)]
		{
			let path = dir.path().join("socket");
			let _socket = std::os::unix::net::UnixListener::bind(&path).unwrap();
			assert!(load_or_create(&path, KEY).await.is_err());

			use std::os::unix::ffi::OsStrExt;
			let path = dir.path().join("fifo");
			let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
			// SAFETY: name is a valid, NUL-terminated path and mkfifo retains no pointer.
			assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
			assert!(load_or_create(&path, KEY).await.is_err());
		}
	}

	#[test]
	fn debug_never_prints_the_bytes() {
		let secret = Secret::from([0xab; 32]);
		assert_eq!(format!("{secret:?}"), "Secret([redacted])");
	}

	/// The file `openssl rand -hex 32 > secret.key` writes decodes with or
	/// without its newline, and anything else is refused.
	#[test]
	fn read_requires_exactly_64_hex_characters() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("secret.key");

		for valid in [format!("{KEY}\n"), KEY.to_string()] {
			std::fs::write(&path, valid).unwrap();
			let secret = Secret::read(&path).unwrap();
			assert_eq!(secret.as_bytes()[31], 0x1f);
		}

		// Empty, short, raw bytes, non-hex, and a byte too few or too many.
		let invalid: [&[u8]; 6] = [b"", b"\n", &[0x5a; 32], &[b'x'; 64], &[b'0'; 62], &[b'0'; 66]];
		for contents in invalid {
			std::fs::write(&path, contents).unwrap();
			assert!(matches!(Secret::read(&path), Err(Error::Invalid(_))), "{contents:?}");
		}
		for contents in [format!("{KEY}\n\n"), format!(" {KEY}"), format!("{KEY}\r\n")] {
			std::fs::write(&path, &contents).unwrap();
			assert!(matches!(Secret::read(&path), Err(Error::Invalid(_))), "{contents:?}");
		}

		assert!(matches!(
			Secret::read(dir.path().join("missing.key")),
			Err(Error::Read(..))
		));
	}
}
