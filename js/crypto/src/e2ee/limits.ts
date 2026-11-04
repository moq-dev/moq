/** Profile string, also the HKDF salt. */
export const PROFILE = "moq-e2ee-00";

/** HKDF info prefix for opaque broadcast paths. */
export const PATH_LABEL = "moq-e2ee-00 path";

/** HKDF info prefix for the HMAC-SHA-256 key that derives physical name nonces. */
export const NAME_MAC_LABEL = "moq-e2ee-00 name-mac";

/** HKDF info prefix for the AES-128-GCM key that seals physical names. */
export const NAME_KEY_LABEL = "moq-e2ee-00 name-key";

/** HKDF info prefix for AEAD keys. */
export const KEY_LABEL = "moq-e2ee-00 key";

/** AES-GCM tag length in bytes. */
export const TAG_LEN = 16;

/** Derived AEAD key length in bytes. */
export const KEY_LEN = 16;

/** Opaque path material length in bytes. */
export const PATH_LEN = 16;

/** HMAC-SHA-256 name key length in bytes. */
export const NAME_MAC_LEN = 32;

/** Leading HMAC bytes a physical name uses as its AES-GCM nonce. */
export const NAME_NONCE_LEN = 12;

/** Padded semantic names are a multiple of this many bytes. */
export const NAME_BLOCK = 16;

/** Broadcast secret length in bytes. */
export const SECRET_LEN = 32;

/** `Number.MAX_SAFE_INTEGER`: the group and kid bound. */
export const MAX_U53 = Number.MAX_SAFE_INTEGER;

/** The frame bound: the nonce's 32-bit half. */
export const MAX_U32 = 0xffff_ffff;

/** Maximum AEAD invocations per key (`2^24`). */
export const MAX_INVOCATIONS = 2 ** 24;

/** Maximum plaintext bytes per key (`2^36`). */
export const MAX_PLAINTEXT_BYTES = 2 ** 36;

/** Interoperable grouped-frame payload cap matching moq-net: 32 MiB. */
export const MAX_GROUPED_PAYLOAD = 32 * 1024 * 1024;

/** Largest plaintext a grouped frame can carry: 32 MiB minus the tag. */
export const MAX_GROUPED_PLAINTEXT = MAX_GROUPED_PAYLOAD - TAG_LEN;

/** moq-lite datagram body cap, including Type, Subscribe ID, Group Sequence, and Timestamp. */
export const MAX_DATAGRAM_BODY = 1200;

/** Widest moq-lite datagram header: lite-07's one-byte Type and three 9-byte leading-ones varints. */
export const MAX_DATAGRAM_HEADER = 28;

/** Largest protected datagram payload: the body cap minus the widest header. */
export const MAX_DATAGRAM_PAYLOAD = MAX_DATAGRAM_BODY - MAX_DATAGRAM_HEADER;

/** Largest plaintext a datagram can carry: 1156 bytes. */
export const MAX_DATAGRAM_PLAINTEXT = MAX_DATAGRAM_PAYLOAD - TAG_LEN;

/** Datagram duplicate suppression window, in sequences below the greatest opened. */
export const DATAGRAM_WINDOW = 1024;

/** Maximum length of a `bytes` field: context, epoch, or semantic name. */
export const MAX_BYTES = 0xffff;
