import { Failure } from "./failure.ts";
import { KEY_LEN, PROFILE, SECRET_LEN } from "./limits.ts";

const SALT = new TextEncoder().encode(PROFILE);

// Each credential's secret as a nonextractable HKDF key, kept off the public object.
const secrets = new WeakMap<object, Promise<CryptoKey>>();

/**
 * Take `secret` for `owner`: 32 bytes, imported into a nonextractable key, or such a key.
 *
 * Throws {@link Failure} `invalid_secret` for anything else.
 */
export function bind(owner: object, secret: Uint8Array | CryptoKey): void {
	const key = importSecret(secret);
	// Every derivation awaits this and reports the failure itself.
	key.catch(() => {});
	secrets.set(owner, key);
}

function importSecret(secret: Uint8Array | CryptoKey): Promise<CryptoKey> {
	if (secret instanceof Uint8Array) {
		if (secret.byteLength !== SECRET_LEN) throw new Failure("invalid_secret");
		const copy = secret.slice();
		return crypto.subtle.importKey("raw", copy, "HKDF", false, ["deriveBits"]).finally(() => copy.fill(0));
	}
	if (
		typeof CryptoKey === "undefined" ||
		!(secret instanceof CryptoKey) ||
		secret.algorithm.name !== "HKDF" ||
		secret.extractable ||
		!secret.usages.includes("deriveBits")
	) {
		throw new Failure("invalid_secret");
	}
	return Promise.resolve(secret);
}

/** HKDF-SHA-256 under `owner`'s secret with the profile salt: `length` bytes of output for `info`. */
export async function expand(
	owner: object,
	info: Uint8Array<ArrayBuffer>,
	length = KEY_LEN,
): Promise<Uint8Array<ArrayBuffer>> {
	const secret = secrets.get(owner);
	if (!secret) throw new Failure("invalid_secret");
	const bits = await crypto.subtle.deriveBits(
		{ name: "HKDF", hash: "SHA-256", salt: SALT, info },
		await secret,
		length * 8,
	);
	return new Uint8Array(bits);
}
