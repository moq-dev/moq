/**
 * Endpoint keys: an Ed25519 secret key and the public key that names its endpoint.
 *
 * @module
 */
import { Failure } from "./failure.ts";
import * as ZBase32 from "./zbase32.ts";

const KEY_LEN = 32;
const ED25519 = { name: "Ed25519" } as const;
// The PKCS #8 wrapper for a raw 32-byte Ed25519 seed, the only private format WebCrypto imports.
const PKCS8_PREFIX = Uint8Array.from([
	0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
]);

/**
 * An endpoint's Ed25519 public key, which is also its endpoint ID.
 *
 * Its text form is the 52-character z-base-32 of the key, the form pkarr and iroh's DNS
 * discovery use, which is also a valid DNS label. Parsing accepts only that form (lowercase,
 * zero trailing bits), so one key has exactly one ID to name a path by.
 */
export class PublicKey {
	#bytes: Uint8Array<ArrayBuffer>;
	#verifier?: Promise<CryptoKey>;

	private constructor(bytes: Uint8Array<ArrayBuffer>) {
		this.#bytes = bytes;
	}

	/** Wrap the 32 raw bytes of an Ed25519 public key, copying them; throws {@link Failure} `key` otherwise. @internal */
	static fromBytes(bytes: Uint8Array): PublicKey {
		if (bytes.byteLength !== KEY_LEN) throw new Failure("key");
		return new PublicKey(Uint8Array.from(bytes));
	}

	/** Parse an endpoint ID; throws {@link Failure} `key` unless it is 52 z-base-32 characters. */
	static parse(text: string): PublicKey {
		const bytes = ZBase32.decode(text, KEY_LEN);
		if (!bytes) throw new Failure("key");
		return new PublicKey(bytes);
	}

	/** Whether `other` is the same key. */
	equals(other: PublicKey): boolean {
		return this.#bytes.every((byte, i) => byte === other.#bytes[i]);
	}

	/** The endpoint ID: 52 z-base-32 characters. */
	toString(): string {
		return ZBase32.encode(this.#bytes);
	}

	/** Whether `signature` is this key's Ed25519 signature over `message`. @internal */
	async verify(message: Uint8Array<ArrayBuffer>, signature: Uint8Array<ArrayBuffer>): Promise<boolean> {
		this.#verifier ??= crypto.subtle.importKey("raw", this.#bytes, ED25519, false, ["verify"]);
		return crypto.subtle.verify(ED25519, await this.#verifier, signature, message);
	}
}

/** An endpoint's Ed25519 secret key. The private half stays a nonextractable `CryptoKey`. */
export class SecretKey {
	/** The public key, which is this endpoint's ID. */
	readonly public: PublicKey;
	#signer: CryptoKey;

	private constructor(signer: CryptoKey, publicKey: PublicKey) {
		this.#signer = signer;
		this.public = publicKey;
	}

	/** Generate a fresh key from the platform's secure random source. */
	static async generate(): Promise<SecretKey> {
		const pair = await crypto.subtle.generateKey(ED25519, false, ["sign", "verify"]);
		const raw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
		return new SecretKey(pair.privateKey, PublicKey.fromBytes(raw));
	}

	/** Load the key for a 32-byte seed, as the Rust key file stores it. @internal */
	static async fromSeed(seed: Uint8Array): Promise<SecretKey> {
		if (seed.byteLength !== KEY_LEN) throw new Failure("key");
		const pkcs8 = new Uint8Array(PKCS8_PREFIX.byteLength + KEY_LEN);
		pkcs8.set(PKCS8_PREFIX);
		pkcs8.set(seed, PKCS8_PREFIX.byteLength);
		const signer = await crypto.subtle.importKey("pkcs8", pkcs8, ED25519, false, ["sign"]);
		// An extractable copy only to read the public half back out.
		const exported = await crypto.subtle.importKey("pkcs8", pkcs8, ED25519, true, ["sign"]);
		const { x } = await crypto.subtle.exportKey("jwk", exported);
		pkcs8.fill(0);
		const raw = Uint8Array.from(atob((x ?? "").replaceAll("-", "+").replaceAll("_", "/")), (c) => c.charCodeAt(0));
		return new SecretKey(signer, PublicKey.fromBytes(raw));
	}

	/** Sign `message` with Ed25519. @internal */
	async sign(message: Uint8Array<ArrayBuffer>): Promise<Uint8Array<ArrayBuffer>> {
		return new Uint8Array(await crypto.subtle.sign(ED25519, this.#signer, message));
	}
}
