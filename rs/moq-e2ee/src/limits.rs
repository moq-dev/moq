/// Profile string, also the HKDF salt.
pub(crate) const PROFILE: &str = "moq-e2ee-00";

/// HKDF info prefix for opaque broadcast paths.
pub(crate) const PATH_LABEL: &[u8] = b"moq-e2ee-00 path";

/// HKDF info prefix for physical names.
pub(crate) const NAME_LABEL: &[u8] = b"moq-e2ee-00 name";

/// HKDF info prefix for AEAD keys.
pub(crate) const KEY_LABEL: &[u8] = b"moq-e2ee-00 key";

/// AES-GCM tag length in bytes.
pub(crate) const TAG_LEN: usize = 16;

/// Derived AEAD key length in bytes.
pub(crate) const KEY_LEN: usize = 16;

/// Physical name and opaque path material length in bytes.
pub(crate) const NAME_LEN: usize = 16;

/// Unpadded base64url length of 16 bytes, in ASCII characters.
pub(crate) const NAME_TEXT_LEN: usize = 22;

/// Broadcast secret length in bytes.
pub const SECRET_LEN: usize = 32;

/// `Number.MAX_SAFE_INTEGER`: the group and kid bound.
pub(crate) const MAX_U53: u64 = 9_007_199_254_740_991;

/// Maximum AEAD invocations per key (`2^24`).
pub(crate) const MAX_INVOCATIONS: u64 = 16_777_216;

/// Maximum plaintext bytes per key (`2^36`).
pub(crate) const MAX_PLAINTEXT_BYTES: u64 = 68_719_476_736;

/// Interoperable grouped-frame payload cap matching moq-net: 32 MiB.
pub(crate) const MAX_GROUPED_PAYLOAD: usize = 32 * 1024 * 1024;

/// Largest plaintext a grouped frame can carry: 32 MiB minus the tag.
pub const MAX_GROUPED_PLAINTEXT: usize = MAX_GROUPED_PAYLOAD - TAG_LEN;

/// moq-lite datagram body cap, including Subscribe ID, Group Sequence, and Timestamp.
pub(crate) const MAX_DATAGRAM_BODY: usize = 1200;

/// Widest moq-lite datagram header: three 8-byte QUIC varints.
pub(crate) const MAX_DATAGRAM_HEADER: usize = 24;

/// Largest protected datagram payload: the body cap minus the widest header.
pub(crate) const MAX_DATAGRAM_PAYLOAD: usize = MAX_DATAGRAM_BODY - MAX_DATAGRAM_HEADER;

/// Largest plaintext a datagram can carry: 1160 bytes.
pub const MAX_DATAGRAM_PLAINTEXT: usize = MAX_DATAGRAM_PAYLOAD - TAG_LEN;

/// Datagram duplicate suppression window, in sequences below the greatest opened.
pub(crate) const DATAGRAM_WINDOW: u64 = 1024;

/// Maximum length of a `bytes` field: context, epoch, or semantic name.
pub(crate) const MAX_BYTES: usize = 0xffff;

pub(crate) fn check_u53(value: u64) -> crate::Result<()> {
	if value > MAX_U53 {
		Err(crate::Error::Identity)
	} else {
		Ok(())
	}
}

pub(crate) fn check_bytes(len: usize) -> crate::Result<()> {
	if len > MAX_BYTES {
		Err(crate::Error::Identity)
	} else {
		Ok(())
	}
}
