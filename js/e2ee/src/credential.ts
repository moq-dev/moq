import {
	DOMAIN_DATAGRAM,
	DOMAIN_GROUP,
	type Domain,
	KEY_LABEL,
	KEY_LEN,
	MAX_INVOCATIONS,
	MAX_PLAINTEXT_BYTES,
	NAME_LABEL,
	NAME_LEN,
	PROFILE,
	SALT,
	SECRET_LEN,
	TAG_LEN,
} from "./constants.ts";
import {
	base64url,
	checkU53,
	concat,
	contextBytes,
	copy,
	encodeBytes,
	encodeU64,
	encodeUtf8,
	nonce,
} from "./encoding.ts";
import { Failure } from "./error.ts";

/** A 32-byte IKM or a nonextractable WebCrypto HKDF key derived from one. */
export type Secret = Uint8Array | CryptoKey;

/** Application pin: refuse a credential that is not this profile/generation/kid. */
export interface Pin {
	/** Expected profile; defaults to `moq-e2ee-01`. */
	profile?: string;
	/** Expected generation. */
	generation: number;
	/** Expected kid. */
	kid: number;
}

/** Inputs for {@link Credential}. */
export interface Init {
	/** Profile string; defaults to `moq-e2ee-01`. */
	profile?: string;
	/** Broadcast identity bytes, or a UTF-8 string. */
	context: Uint8Array | string;
	/** Rotation counter. Never changes in place. */
	generation: number;
	/** Selects among credentials the application retains. Never changes in place. */
	kid: number;
	/** 32-byte secret or an HKDF `CryptoKey` imported from one. */
	secret: Secret;
	/** If set, the credential must match this pin. */
	pin?: Pin;
}

type KeySlot = {
	crypto: CryptoKey;
	invocations: number;
	bytes: number;
	reservedInvocations: number;
	reservedBytes: number;
	used: Set<string>;
	inflight: Set<string>;
	ciphertexts: Map<string, Uint8Array>;
};

function identityKey(group: number, frame: number): string {
	return `${group}:${frame}`;
}

function slotKey(physicalName: string, domain: Domain): string {
	return `${domain}:${physicalName}`;
}

function emptyAad(): Uint8Array<ArrayBuffer> {
	return new Uint8Array(0);
}

/**
 * Immutable broadcast credential: profile, context, generation, kid, and secret.
 *
 * The secret is imported as a nonextractable HKDF key and never serialized. One credential
 * authorizes every track in the generation; per-track keys and used identities live here.
 */
export class Credential {
	/** Profile this credential names. */
	readonly profile: string;
	/** Broadcast identity bytes. */
	readonly context: Uint8Array;
	/** Rotation counter. */
	readonly generation: number;
	/** Key identifier. */
	readonly kid: number;

	#ikm?: CryptoKey;
	#secret: Secret;
	#keys = new Map<string, KeySlot>();
	#names = new Map<string, string>();
	#publishers = new Set<string>();

	/** Validate and hold a credential. Derivation happens on first use. */
	constructor(init: Init) {
		this.profile = init.profile ?? PROFILE;
		if (this.profile !== PROFILE) throw new Failure("unsupported_profile");
		this.context = contextBytes(init.context);
		checkU53(init.generation, "generation");
		checkU53(init.kid, "kid");
		this.generation = init.generation;
		this.kid = init.kid;
		this.#secret = checkSecret(init.secret);
		if (init.pin) {
			const profile = init.pin.profile ?? PROFILE;
			if (profile !== this.profile || init.pin.generation !== this.generation || init.pin.kid !== this.kid) {
				throw new Failure("pinned_mismatch");
			}
		}
	}

	/** Derive the 22-character opaque physical name for `semanticName`. */
	async opaqueName(semanticName: string): Promise<string> {
		const cached = this.#names.get(semanticName);
		if (cached) return cached;
		const semantic = encodeUtf8(semanticName);
		if (semantic.length > 0xffff) throw new Failure("identity", "semantic name exceeds 65535 bytes");
		const ikm = await this.#material();
		const info = concat(
			NAME_LABEL,
			encodeBytes(this.context),
			encodeU64(this.generation),
			encodeU64(this.kid),
			encodeBytes(semantic),
		);
		const bits = await crypto.subtle.deriveBits(
			{ name: "HKDF", hash: "SHA-256", salt: copy(SALT), info: copy(info) },
			ikm,
			NAME_LEN * 8,
		);
		const physical = base64url(new Uint8Array(bits));
		this.#names.set(semanticName, physical);
		return physical;
	}

	/**
	 * Encrypt `plaintext` at this identity. Same identity with different bytes is `reuse`;
	 * retransmission reads {@link ciphertext} instead of calling this again.
	 */
	async seal(input: {
		physicalName: string;
		domain: Domain;
		group: number;
		frame: number;
		plaintext: Uint8Array;
		payloadLimit: number;
	}): Promise<Uint8Array> {
		if (input.domain !== DOMAIN_GROUP && input.domain !== DOMAIN_DATAGRAM) {
			throw new Failure("identity", "domain is not grouped or datagram");
		}
		if (input.plaintext.length + TAG_LEN > input.payloadLimit) throw new Failure("oversize");
		const slot = await this.#slot(input.physicalName, input.domain);
		const id = identityKey(input.group, input.frame);
		this.#reserve(slot, id, input.plaintext.length);
		try {
			const iv = nonce(input.group, input.frame);
			const sealed = new Uint8Array(
				await crypto.subtle.encrypt(
					{ name: "AES-GCM", iv: copy(iv), tagLength: 128, additionalData: emptyAad() },
					slot.crypto,
					copy(input.plaintext),
				),
			);
			slot.used.add(id);
			slot.ciphertexts.set(id, sealed);
			slot.invocations++;
			slot.bytes += input.plaintext.length;
			return sealed;
		} finally {
			slot.inflight.delete(id);
			slot.reservedInvocations--;
			slot.reservedBytes -= input.plaintext.length;
		}
	}

	/**
	 * Decrypt `payload` at this identity. Authentication failure is `authentication`.
	 * Counts against the per-key invocation and plaintext-byte caps.
	 */
	async open(input: {
		physicalName: string;
		domain: Domain;
		group: number;
		frame: number;
		payload: Uint8Array;
		payloadLimit: number;
	}): Promise<Uint8Array> {
		if (input.domain !== DOMAIN_GROUP && input.domain !== DOMAIN_DATAGRAM) {
			throw new Failure("identity", "domain is not grouped or datagram");
		}
		if (input.payload.length < TAG_LEN || input.payload.length > input.payloadLimit) {
			throw new Failure("oversize");
		}
		const slot = await this.#slot(input.physicalName, input.domain);
		if (slot.invocations + slot.reservedInvocations + 1 > MAX_INVOCATIONS) {
			throw new Failure("exhausted");
		}
		const iv = nonce(input.group, input.frame);
		slot.reservedInvocations++;
		try {
			let opened: Uint8Array;
			try {
				opened = new Uint8Array(
					await crypto.subtle.decrypt(
						{ name: "AES-GCM", iv: copy(iv), tagLength: 128, additionalData: emptyAad() },
						slot.crypto,
						copy(input.payload),
					),
				);
			} catch {
				slot.invocations++;
				throw new Failure("authentication");
			}
			if (slot.bytes + slot.reservedBytes + opened.length > MAX_PLAINTEXT_BYTES) {
				slot.invocations++;
				throw new Failure("exhausted");
			}
			slot.invocations++;
			slot.bytes += opened.length;
			return opened;
		} finally {
			slot.reservedInvocations--;
		}
	}

	/** Ciphertext previously produced at this identity, if this credential still holds it. */
	ciphertext(physicalName: string, domain: Domain, group: number, frame: number): Uint8Array | undefined {
		return this.#keys.get(slotKey(physicalName, domain))?.ciphertexts.get(identityKey(group, frame));
	}

	/**
	 * Claim exclusive publication of `physicalName` for this generation.
	 * A second claim is a same-generation restart and is `reuse`.
	 */
	claimPublisher(physicalName: string): void {
		if (this.#publishers.has(physicalName)) {
			throw new Failure("reuse", "same-generation restart refused");
		}
		this.#publishers.add(physicalName);
	}

	/**
	 * Pretend this key has already performed `invocations` AEAD ops totaling `bytes` plaintext.
	 *
	 * @internal Tests only.
	 */
	async primeUsage(physicalName: string, domain: Domain, invocations: number, bytes: number): Promise<void> {
		const slot = await this.#slot(physicalName, domain);
		slot.invocations = invocations;
		slot.bytes = bytes;
	}

	async #material(): Promise<CryptoKey> {
		if (this.#ikm) return this.#ikm;
		this.#ikm = await importSecret(this.#secret);
		if (this.#secret instanceof Uint8Array) this.#secret.fill(0);
		this.#secret = this.#ikm;
		return this.#ikm;
	}

	async #slot(physicalName: string, domain: Domain): Promise<KeySlot> {
		const key = slotKey(physicalName, domain);
		const existing = this.#keys.get(key);
		if (existing) return existing;
		if (physicalName.length > 0xffff) throw new Failure("identity", "physical name exceeds 65535 bytes");
		const ikm = await this.#material();
		const info = concat(
			KEY_LABEL,
			encodeBytes(this.context),
			encodeU64(this.generation),
			encodeU64(this.kid),
			encodeBytes(encodeUtf8(physicalName)),
			new Uint8Array([domain]),
		);
		const bits = await crypto.subtle.deriveBits(
			{ name: "HKDF", hash: "SHA-256", salt: copy(SALT), info: copy(info) },
			ikm,
			KEY_LEN * 8,
		);
		const cryptoKey = await crypto.subtle.importKey("raw", bits, "AES-GCM", false, ["encrypt", "decrypt"]);
		const slot: KeySlot = {
			crypto: cryptoKey,
			invocations: 0,
			bytes: 0,
			reservedInvocations: 0,
			reservedBytes: 0,
			used: new Set(),
			inflight: new Set(),
			ciphertexts: new Map(),
		};
		this.#keys.set(key, slot);
		return slot;
	}

	#reserve(slot: KeySlot, id: string, plaintextLen: number): void {
		if (slot.used.has(id) || slot.inflight.has(id)) throw new Failure("reuse");
		if (
			slot.invocations + slot.reservedInvocations + 1 > MAX_INVOCATIONS ||
			slot.bytes + slot.reservedBytes + plaintextLen > MAX_PLAINTEXT_BYTES
		) {
			throw new Failure("exhausted");
		}
		slot.inflight.add(id);
		slot.reservedInvocations++;
		slot.reservedBytes += plaintextLen;
	}
}

function checkSecret(secret: Secret): Secret {
	if (secret instanceof CryptoKey) {
		if (secret.algorithm.name !== "HKDF") {
			throw new Failure("invalid_secret", "WebCrypto secret must be an HKDF key");
		}
		return secret;
	}
	if (secret.byteLength !== SECRET_LEN) throw new Failure("invalid_secret");
	return copy(secret);
}

async function importSecret(secret: Secret): Promise<CryptoKey> {
	if (secret instanceof CryptoKey) return secret;
	return crypto.subtle.importKey("raw", copy(secret), "HKDF", false, ["deriveBits"]);
}
