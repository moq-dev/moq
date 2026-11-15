use zeroize::Zeroizing;

use crate::endpoint::SecretKey;
use crate::endpoint::key::ZBASE32;

/// RFC 8032's test 1 seed and the endpoint ID of its public key.
const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const ENDPOINT: &str = "47pjoycnsrfmxikm95jh13y88e8qnhzu5kungjpxyepgt7a8krpy";

fn seed() -> Zeroizing<[u8; 32]> {
	Zeroizing::new(hex::decode(SEED).unwrap().try_into().unwrap())
}

#[test]
fn seed_derives_its_endpoint_id() {
	assert_eq!(SecretKey::from_seed(&seed()).public().to_string(), ENDPOINT);
}

#[cfg(unix)]
#[test]
fn key_file_loads_its_seed() {
	use std::os::unix::fs::PermissionsExt;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("endpoint.key");
	std::fs::write(&path, format!("{}\n", ZBASE32.encode(seed().as_ref()))).unwrap();
	std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
	assert_eq!(
		SecretKey::load_or_generate(&path).unwrap().public().to_string(),
		ENDPOINT
	);
}

#[test]
fn key_file_persists_and_never_rotates() {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("endpoint.key");

	let created = SecretKey::load_or_generate(&path).unwrap();
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
	}
	let loaded = SecretKey::load_or_generate(&path).unwrap();
	assert_eq!(created.public(), loaded.public());
	// The seed as 52 z-base-32 characters and a newline.
	let text = std::fs::read_to_string(&path).unwrap();
	let encoded = text.strip_suffix('\n').unwrap();
	assert_eq!(encoded.len(), 52);
	assert!(
		encoded
			.bytes()
			.all(|b| b"ybndrfg8ejkmcpqxot1uwisza345h769".contains(&b)),
		"{text}"
	);

	// A damaged file is an error, never a reason to mint a new identity.
	std::fs::write(&path, "not a key\n").unwrap();
	let err = SecretKey::load_or_generate(&path).unwrap_err();
	assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
	assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a key\n");
}

#[cfg(unix)]
#[test]
fn key_file_readable_by_others_is_refused() {
	use std::os::unix::fs::PermissionsExt;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("endpoint.key");
	SecretKey::load_or_generate(&path).unwrap();
	std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
	let err = SecretKey::load_or_generate(&path).unwrap_err();
	assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn debug_redacts_the_seed() {
	let key = SecretKey::from_seed(&[0x42; 32]);
	let debug = format!("{key:?}");
	assert!(!debug.contains(&"42".repeat(32)), "{debug}");
	assert!(debug.contains("<redacted>"), "{debug}");
	assert!(debug.contains(&key.public().to_string()));
}

#[test]
fn route_key_is_the_same_key() {
	let key = SecretKey::from_seed(&seed());
	let route = moq_net::origin::RouteKey::from(&key);
	let expected = moq_net::origin::RouteKey::from_seed(&seed());
	assert_eq!(format!("{route:?}"), format!("{expected:?}"));
}
