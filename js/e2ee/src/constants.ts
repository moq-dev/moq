/** Profile identifier this package implements. */
export const PROFILE = "moq-e2ee-01";

/** HKDF salt: the ASCII bytes of {@link PROFILE}. */
export const SALT = new TextEncoder().encode(PROFILE);

/** HKDF info prefix for physical names. */
export const NAME_LABEL = new TextEncoder().encode("moq-e2ee-01 name");

/** HKDF info prefix for AEAD keys. */
export const KEY_LABEL = new TextEncoder().encode("moq-e2ee-01 key");

/** Grouped-frame key domain. */
export const DOMAIN_GROUP = 0x00;

/** Datagram key domain. */
export const DOMAIN_DATAGRAM = 0x01;

/** AES-GCM tag length in bytes. */
export const TAG_LEN = 16;

/** AES-128-GCM key length in bytes. */
export const KEY_LEN = 16;

/** HKDF physical-name material length in bytes. */
export const NAME_LEN = 16;

/** Broadcast secret length in bytes. */
export const SECRET_LEN = 32;

/** Largest integer TypeScript can represent exactly; group, generation, and kid bound. */
export const MAX_U53 = Number.MAX_SAFE_INTEGER;

/** 32-bit frame ID bound. */
export const MAX_U32 = 2 ** 32 - 1;

/** AES-GCM record limit per key. */
export const MAX_INVOCATIONS = 2 ** 24;

/** Total plaintext-byte cap per key. */
export const MAX_PLAINTEXT_BYTES = 2 ** 36;

/** Interoperable grouped-frame payload cap, matching moq-net. */
export const MAX_GROUPED_PAYLOAD = 32 * 1024 * 1024;

/** Maximum grouped plaintext so ciphertext plus tag fits {@link MAX_GROUPED_PAYLOAD}. */
export const MAX_GROUPED_PLAINTEXT = MAX_GROUPED_PAYLOAD - TAG_LEN;

/** moq-lite datagram body cap, including Subscribe ID, sequence, and timestamp. */
export const MAX_DATAGRAM_BODY = 1200;

/**
 * In-flight AEAD operations per producer or consumer before a write waits.
 *
 * 20 ms Opus (160-byte 64 kbps frames) encrypts in well under 1 ms on Bun's WebCrypto
 * and in a Chromium worker. Depth 8 absorbs a ~160 ms capture burst or a main-thread
 * stall without reordering or growing an unbounded promise queue. The wire profile
 * does not depend on this number.
 */
export const DEFAULT_PUMP_DEPTH = 8;

/**
 * Additional waiters allowed behind {@link DEFAULT_PUMP_DEPTH} in-flight ops.
 * A further submit is refused (queue saturation) rather than queued without bound.
 */
export const DEFAULT_PUMP_QUEUE = 8;

/** Receiver datagram duplicate window, in sequences. */
export const DEFAULT_DATAGRAM_WINDOW = 1024;

/** Hang catalog semantic name. */
export const SEMANTIC_HANG_CATALOG = "catalog.json";

/** Compressed Hang catalog semantic name. Compression happens before AEAD. */
export const SEMANTIC_HANG_CATALOG_Z = "catalog.json.z";

/** MSF catalog semantic name. */
export const SEMANTIC_MSF_CATALOG = "catalog";

/** Key domain: grouped frames or datagrams. */
export type Domain = typeof DOMAIN_GROUP | typeof DOMAIN_DATAGRAM;
