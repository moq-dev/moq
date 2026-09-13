/// Profile string `moq-e2ee-01`.
pub const PROFILE: &str = "moq-e2ee-01";

/// HKDF salt: ASCII `moq-e2ee-01`.
pub const SALT: &[u8] = PROFILE.as_bytes();

/// HKDF info prefix for physical names.
pub const NAME_LABEL: &[u8] = b"moq-e2ee-01 name";

/// HKDF info prefix for AEAD keys.
pub const KEY_LABEL: &[u8] = b"moq-e2ee-01 key";

/// AES-GCM tag length in bytes.
pub const TAG_LEN: usize = 16;

/// Derived AEAD key length in bytes.
pub const KEY_LEN: usize = 16;

/// Physical name material length in bytes.
pub const NAME_LEN: usize = 16;

/// Unpadded base64url physical name length in ASCII bytes.
pub const PHYSICAL_NAME_LEN: usize = 22;

/// Broadcast secret length in bytes.
pub const SECRET_LEN: usize = 32;

/// `Number.MAX_SAFE_INTEGER`: group, generation, and kid bound.
pub const MAX_U53: u64 = 9_007_199_254_740_991;

/// Maximum 32-bit frame ID.
pub const MAX_U32: u64 = u32::MAX as u64;

/// Maximum AEAD invocations per key (`2^24`).
pub const MAX_INVOCATIONS: u64 = 16_777_216;

/// Maximum plaintext bytes per key (`2^36`).
pub const MAX_PLAINTEXT_BYTES: u64 = 68_719_476_736;

/// Interoperable grouped-frame payload cap matching moq-net: 32 MiB.
pub const MAX_GROUPED_PAYLOAD: usize = 32 * 1024 * 1024;

/// Maximum grouped plaintext: [`MAX_GROUPED_PAYLOAD`] minus the tag.
pub const MAX_GROUPED_PLAINTEXT: usize = MAX_GROUPED_PAYLOAD - TAG_LEN;

/// moq-lite datagram body cap, including Subscribe ID, Group Sequence, and Timestamp.
pub const MAX_DATAGRAM_BODY: usize = 1200;

/// Smallest datagram header: three one-byte QUIC varints.
pub const MIN_DATAGRAM_HEADER: usize = 3;

/// Duplicate suppression: current grouped track plus the previous group.
pub const GROUP_DUPLICATE_GROUPS: usize = 2;

/// Duplicate suppression: 1024-sequence sliding window for datagrams.
///
/// Chosen to match the profile recommendation. AES-128-GCM of a 160-byte Opus
/// frame is well under a millisecond on one core (see the `protect` bench), so
/// this bounds memory rather than encryption throughput. The wire profile is
/// unchanged.
pub const DATAGRAM_DUPLICATE_WINDOW: usize = 1024;

/// Maximum length of a `bytes` field (context or semantic name).
pub const MAX_BYTES: usize = 0xffff;

/// Grouped-frame key domain.
pub const DOMAIN_GROUP: u8 = 0x00;

/// Datagram key domain.
pub const DOMAIN_DATAGRAM: u8 = 0x01;

/// QUIC varint length of `value`.
pub fn varint_len(value: u64) -> usize {
	if value < 64 {
		1
	} else if value < 16_384 {
		2
	} else if value < 1 << 30 {
		4
	} else {
		8
	}
}

/// Datagram ciphertext budget for the header that object will actually encode.
///
/// # Errors
///
/// [`Error::Identity`](crate::Error::Identity) if any field cannot be a QUIC varint.
pub fn datagram_payload_limit(subscribe: u64, sequence: u64, timestamp: u64) -> crate::Result<usize> {
	if subscribe >= 1 << 62 || sequence >= 1 << 62 || timestamp >= 1 << 62 {
		return Err(crate::Error::Identity);
	}
	let header = varint_len(subscribe) + varint_len(sequence) + varint_len(timestamp);
	Ok(MAX_DATAGRAM_BODY.saturating_sub(header))
}

pub(crate) fn check_u53(value: u64) -> crate::Result<()> {
	if value > MAX_U53 {
		Err(crate::Error::Identity)
	} else {
		Ok(())
	}
}

pub(crate) fn check_frame(frame: u64) -> crate::Result<u32> {
	u32::try_from(frame).map_err(|_| crate::Error::Identity)
}

pub(crate) fn check_bytes(len: usize) -> crate::Result<()> {
	if len > MAX_BYTES {
		Err(crate::Error::Identity)
	} else {
		Ok(())
	}
}
