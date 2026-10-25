import type { Epoch } from "@moq/net";
import type { Credential } from "./credential.ts";
import { decode, encode, type Name, nameKeyInfo, nameMacInfo, pad, unpad } from "./derive.ts";
import { Failure } from "./failure.ts";
import { KEY_LEN, NAME_MAC_LEN, NAME_NONCE_LEN, TAG_LEN } from "./limits.ts";
import { expand } from "./secret.ts";

/** A generation's name keys: an HMAC-SHA-256 key for nonces and an AES-128-GCM key for sealing. */
export class NameKeys {
	#mac: Promise<CryptoKey>;
	#seal: Promise<CryptoKey>;

	constructor(credential: Credential, epoch: Epoch.Valid) {
		const { context, kid } = credential;
		const hmac = { name: "HMAC", hash: "SHA-256" };
		this.#mac = importKey(expand(credential, nameMacInfo(context, epoch, kid), NAME_MAC_LEN), hmac, ["sign"]);
		this.#seal = importKey(expand(credential, nameKeyInfo(context, epoch, kid), KEY_LEN), "AES-GCM", [
			"encrypt",
			"decrypt",
		]);
		// Every seal and open awaits these and reports the failure itself.
		this.#mac.catch(() => {});
		this.#seal.catch(() => {});
	}

	/** The physical name for a semantic name: its HMAC nonce, then the padded name sealed under it. */
	async seal(semantic: string): Promise<Name> {
		const padded = pad(semantic);
		const iv = await this.#nonce(padded);
		const sealed = await crypto.subtle.encrypt(
			{ name: "AES-GCM", iv, tagLength: TAG_LEN * 8 },
			await this.#seal,
			padded,
		);
		const material = new Uint8Array(NAME_NONCE_LEN + sealed.byteLength);
		material.set(iv);
		material.set(new Uint8Array(sealed), NAME_NONCE_LEN);
		return encode(material) as Name;
	}

	/**
	 * Recover the semantic name of a physical name this generation issued.
	 *
	 * Rejects with {@link Failure} `identity` if `name` does not parse, and `authentication` if it does
	 * not open, its nonce is not the HMAC of its plaintext, or its padding or UTF-8 is not canonical.
	 */
	async open(name: string): Promise<string> {
		const material = decode(name);
		const iv = material.slice(0, NAME_NONCE_LEN);
		let padded: Uint8Array<ArrayBuffer>;
		try {
			padded = new Uint8Array(
				await crypto.subtle.decrypt(
					{ name: "AES-GCM", iv, tagLength: TAG_LEN * 8 },
					await this.#seal,
					material.subarray(NAME_NONCE_LEN),
				),
			);
		} catch {
			throw new Failure("authentication");
		}
		// One physical name per semantic name, so two tracks never share its payload keys.
		const expected = await this.#nonce(padded);
		if (!expected.every((byte, i) => byte === iv[i])) throw new Failure("authentication");
		return unpad(padded);
	}

	async #nonce(padded: Uint8Array<ArrayBuffer>): Promise<Uint8Array<ArrayBuffer>> {
		return new Uint8Array(await crypto.subtle.sign("HMAC", await this.#mac, padded), 0, NAME_NONCE_LEN);
	}
}

async function importKey(
	bytes: Promise<Uint8Array<ArrayBuffer>>,
	algorithm: AlgorithmIdentifier | HmacImportParams,
	usages: KeyUsage[],
): Promise<CryptoKey> {
	const raw = await bytes;
	try {
		return await crypto.subtle.importKey("raw", raw, algorithm, false, usages);
	} finally {
		raw.fill(0);
	}
}
