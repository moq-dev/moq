/**
 * End-to-end encryption for MoQ tracks, implementing `moq-e2ee-01`.
 *
 * A {@link Credential} holds the broadcast secret (raw bytes or a nonextractable HKDF
 * `CryptoKey`) and derives opaque physical names plus per-track AES-128-GCM keys.
 * {@link Producer} and {@link Consumer} wrap whole group and datagram lifecycles so every
 * AEAD call has the canonical physical name and transport identity. Catalogs and names
 * use {@link opaqueName}, {@link protectCatalog}, and {@link openCatalog}.
 *
 * Keys never enter `@moq/net`. There is no plaintext fallback: grouped authentication
 * failure ends the track; a bad datagram is dropped with a typed event.
 *
 * @module
 */

export {
	CatalogName,
	openCatalog,
	type ProtectedCatalog,
	protectCatalog,
} from "./catalog.ts";
export {
	DEFAULT_DATAGRAM_WINDOW,
	DEFAULT_PUMP_DEPTH,
	DEFAULT_PUMP_QUEUE,
	DOMAIN_DATAGRAM,
	DOMAIN_GROUP,
	type Domain,
	KEY_LEN,
	MAX_DATAGRAM_BODY,
	MAX_GROUPED_PAYLOAD,
	MAX_GROUPED_PLAINTEXT,
	MAX_INVOCATIONS,
	MAX_PLAINTEXT_BYTES,
	MAX_U32,
	MAX_U53,
	NAME_LEN,
	PROFILE,
	SECRET_LEN,
	SEMANTIC_HANG_CATALOG,
	SEMANTIC_HANG_CATALOG_Z,
	SEMANTIC_MSF_CATALOG,
	TAG_LEN,
} from "./constants.ts";
export { Credential, type Init, type Pin, type Secret } from "./credential.ts";
export {
	type DatagramHeader,
	datagramHeaderSize,
	datagramPayloadLimit,
	datagramPlaintextLimit,
} from "./datagram.ts";
export { type Code, Failure, isFailure } from "./error.ts";
export { GroupConsumer, GroupProducer } from "./group.ts";
export { opaqueName, open, protect } from "./primitives.ts";
export {
	Consumer,
	type ConsumerOptions,
	type DatagramEvent,
	Producer,
	type ProducerOptions,
	type SealedFrame,
} from "./track.ts";
