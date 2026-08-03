//! Writing key files without handing the private half to every user on the box.

use std::io;
use std::path::Path;

/// Owner read/write, nobody else.
#[cfg(unix)]
const PRIVATE_MODE: u32 = 0o600;

/// Write a key file, restricting it to the owner when `private` (it holds secret material).
///
/// A public key keeps the default permissions, since it's meant to be handed around and often
/// gets read by a relay running as a different user.
pub(crate) fn write(path: &Path, contents: &str, private: bool) -> io::Result<()> {
	match private {
		true => write_private(path, contents),
		false => std::fs::write(path, contents),
	}
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> io::Result<()> {
	use std::io::Write;
	use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

	let mut file = std::fs::OpenOptions::new()
		.write(true)
		.create(true)
		.truncate(true)
		.mode(PRIVATE_MODE)
		.open(path)?;

	// `mode` above only applies when open(2) creates the file, and it's masked by the umask
	// either way. Set the bits explicitly so overwriting an existing world-readable key
	// tightens it, and do it before write_all so the secret never sits in a readable file.
	file.set_permissions(std::fs::Permissions::from_mode(PRIVATE_MODE))?;
	file.write_all(contents.as_bytes())
}

/// Windows has no cheap equivalent of the Unix mode bits, so the file inherits the directory's ACL.
#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> io::Result<()> {
	std::fs::write(path, contents)
}
