import * as base64 from "@hexagon/base64";
import type { Algorithm } from "./algorithm.ts";
import { type Key, type KeyId, KeyIdSchema } from "./key.ts";

/**
 * Generate a random key ID (16 hex characters).
 */
function randomKid(): KeyId {
	const bytes = new Uint8Array(8);
	crypto.getRandomValues(bytes);
	return Array.from(bytes)
		.map((b) => b.toString(16).padStart(2, "0"))
		.join("") as KeyId;
}

/**
 * Generate a new key for the given algorithm.
 * A random key ID is assigned if none is provided.
 */
export async function generate(algorithm: Algorithm, kid?: string): Promise<Key> {
	const validKid: KeyId = kid?.trim() ? KeyIdSchema.parse(kid.trim()) : randomKid();
	switch (algorithm) {
		case "HS256":
			return generateHmacKey(algorithm, 32, validKid);
		case "HS384":
			return generateHmacKey(algorithm, 48, validKid);
		case "HS512":
			return generateHmacKey(algorithm, 64, validKid);
		case "RS256":
		case "RS384":
		case "RS512":
			return generateRsaKey(algorithm, "RSASSA-PKCS1-v1_5", validKid);
		case "PS256":
		case "PS384":
		case "PS512":
			return generateRsaKey(algorithm, "RSA-PSS", validKid);
		case "ES256":
			return generateEcKey(algorithm, "P-256", validKid);
		case "ES384":
			return generateEcKey(algorithm, "P-384", validKid);
		case "EdDSA":
			return generateEdDsaKey(algorithm, validKid);
		default:
			throw new Error(`Unsupported algorithm: ${algorithm}`);
	}
}

/**
 * Generate an HMAC symmetric key
 */
async function generateHmacKey(alg: Algorithm, byteLength: number, kid: KeyId): Promise<Key> {
	const bytes = new Uint8Array(byteLength);
	crypto.getRandomValues(bytes);

	const k = base64.fromArrayBuffer(bytes.buffer, true);

	return {
		kty: "oct",
		alg,
		k,
		kid,
		key_ops: ["sign", "verify"],
	};
}

/**
 * Generate an RSA asymmetric key pair
 */
async function generateRsaKey(alg: Algorithm, name: "RSASSA-PKCS1-v1_5" | "RSA-PSS", kid: KeyId): Promise<Key> {
	const keyPair = await crypto.subtle.generateKey(
		{
			name,
			modulusLength: 2048,
			publicExponent: new Uint8Array([1, 0, 1]), // 65537
			hash: getHashForAlgorithm(alg),
		},
		true,
		["sign", "verify"],
	);

	const privateKey = "privateKey" in keyPair ? keyPair.privateKey : keyPair;
	const jwk = (await crypto.subtle.exportKey("jwk", privateKey)) as {
		kty: "RSA";
		n: string;
		e: string;
		d: string;
		p: string;
		q: string;
		dp: string;
		dq: string;
		qi: string;
	};

	return {
		kty: "RSA",
		alg,
		n: jwk.n,
		e: jwk.e,
		d: jwk.d,
		p: jwk.p,
		q: jwk.q,
		dp: jwk.dp,
		dq: jwk.dq,
		qi: jwk.qi,
		kid,
		key_ops: ["sign", "verify"],
	};
}

/**
 * Generate an elliptic curve asymmetric key pair
 */
async function generateEcKey(alg: "ES256" | "ES384", namedCurve: "P-256" | "P-384", kid: KeyId): Promise<Key> {
	const keyPair = await crypto.subtle.generateKey(
		{
			name: "ECDSA",
			namedCurve,
		},
		true,
		["sign", "verify"],
	);

	const privateKey = "privateKey" in keyPair ? keyPair.privateKey : keyPair;
	const jwk = (await crypto.subtle.exportKey("jwk", privateKey)) as {
		kty: "EC";
		crv: "P-256" | "P-384";
		x: string;
		y: string;
		d: string;
	};

	return {
		kty: "EC",
		alg,
		crv: jwk.crv,
		x: jwk.x,
		y: jwk.y,
		d: jwk.d,
		kid,
		key_ops: ["sign", "verify"],
	};
}

/**
 * Generate an EdDSA key pair using Ed25519
 */
async function generateEdDsaKey(alg: "EdDSA", kid: KeyId): Promise<Key> {
	const keyPair = await crypto.subtle.generateKey(
		{
			name: "Ed25519",
		},
		true,
		["sign", "verify"],
	);

	const privateKey = "privateKey" in keyPair ? keyPair.privateKey : keyPair;
	const jwk = (await crypto.subtle.exportKey("jwk", privateKey)) as {
		kty: "OKP";
		crv: "Ed25519";
		x: string;
		d: string;
	};

	return {
		kty: "OKP",
		alg,
		crv: "Ed25519",
		x: jwk.x,
		d: jwk.d,
		kid,
		key_ops: ["sign", "verify"],
	};
}

/**
 * Get the hash algorithm for a given JWT algorithm
 */
function getHashForAlgorithm(alg: Algorithm): "SHA-256" | "SHA-384" | "SHA-512" {
	if (alg.endsWith("256")) return "SHA-256";
	if (alg.endsWith("384")) return "SHA-384";
	if (alg.endsWith("512")) return "SHA-512";
	throw new Error(`Cannot determine hash for algorithm: ${alg}`);
}
