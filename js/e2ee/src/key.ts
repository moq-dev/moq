import type { Epoch } from "@moq/net";
import type { Credential } from "./credential.ts";
import { type Domain, keyInfo, nonce } from "./derive.ts";
import { Failure } from "./failure.ts";
import { MAX_INVOCATIONS, MAX_PLAINTEXT_BYTES, TAG_LEN } from "./limits.ts";
import { expand } from "./secret.ts";

/** The 96-bit nonce of an admitted AEAD operation. */
export type Nonce = Uint8Array<ArrayBuffer>;

/**
 * Per-track, per-domain AES-128-GCM key with invocation and byte accounting.
 *
 * An operation is admitted synchronously, which refuses it or spends the budget, then runs
 * asynchronously. Spending at admission means callers can never overshoot a limit while an
 * AEAD call is in flight.
 */
export class TrackKey {
	#key: Promise<CryptoKey>;
	invocations = 0;
	plaintextBytes = 0;

	constructor(key: Promise<CryptoKey>) {
		this.#key = key;
		// Every encrypt and decrypt awaits this and reports the failure itself.
		this.#key.catch(() => {});
	}

	/** Admit an encryption of `length` plaintext bytes; refusals (identity, oversize, exhausted) spend nothing. */
	admitSeal(group: number, frame: number, length: number, limit: number): Nonce {
		const iv = nonce(group, frame);
		if (length + TAG_LEN > limit) throw new Failure("oversize");
		this.#spend(length);
		return iv;
	}

	/**
	 * Admit a decryption of a `length`-byte payload; refusals (identity, oversize, exhausted) spend nothing.
	 *
	 * A failed open still runs AES-GCM over every block, so admission spends whatever the outcome.
	 */
	admitOpen(group: number, frame: number, length: number, limit: number): Nonce {
		const iv = nonce(group, frame);
		if (length < TAG_LEN || length > limit) throw new Failure("oversize");
		this.#spend(length - TAG_LEN);
		return iv;
	}

	/** Encrypt under an admitted nonce, returning ciphertext followed by the tag. */
	async encrypt(iv: Nonce, plaintext: Uint8Array<ArrayBuffer>): Promise<Uint8Array<ArrayBuffer>> {
		const key = await this.#key;
		return new Uint8Array(
			await crypto.subtle.encrypt({ name: "AES-GCM", iv, tagLength: TAG_LEN * 8 }, key, plaintext),
		);
	}

	/** Decrypt under an admitted nonce; a bad tag is {@link Failure} `authentication`. */
	async decrypt(iv: Nonce, payload: Uint8Array): Promise<Uint8Array<ArrayBuffer>> {
		const key = await this.#key;
		// WebCrypto takes only ArrayBuffer-backed views.
		const data = payload.buffer instanceof ArrayBuffer ? (payload as Uint8Array<ArrayBuffer>) : payload.slice();
		let opened: ArrayBuffer;
		try {
			opened = await crypto.subtle.decrypt({ name: "AES-GCM", iv, tagLength: TAG_LEN * 8 }, key, data);
		} catch {
			throw new Failure("authentication");
		}
		return new Uint8Array(opened);
	}

	#spend(length: number): void {
		if (this.invocations >= MAX_INVOCATIONS) throw new Failure("exhausted");
		if (this.plaintextBytes + length > MAX_PLAINTEXT_BYTES) throw new Failure("exhausted");
		this.invocations++;
		this.plaintextBytes += length;
	}
}

/** The 16 key bytes for one semantic name and domain under a generation. */
export function keyBytes(
	credential: Credential,
	epoch: Epoch.Valid,
	semantic: string,
	domain: Domain,
): Promise<Uint8Array<ArrayBuffer>> {
	return expand(credential, keyInfo(credential.context, epoch, credential.kid, semantic, domain));
}

/** A fresh accounting handle over a nonextractable AES-128-GCM key for one semantic name and domain. */
export function trackKey(credential: Credential, epoch: Epoch.Valid, semantic: string, domain: Domain): TrackKey {
	return new TrackKey(aesKey(credential, epoch, semantic, domain));
}

async function aesKey(
	credential: Credential,
	epoch: Epoch.Valid,
	semantic: string,
	domain: Domain,
): Promise<CryptoKey> {
	const bytes = await keyBytes(credential, epoch, semantic, domain);
	try {
		return await crypto.subtle.importKey("raw", bytes, "AES-GCM", false, ["encrypt", "decrypt"]);
	} finally {
		bytes.fill(0);
	}
}
