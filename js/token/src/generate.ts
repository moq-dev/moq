import * as base64 from "@hexagon/base64";
import type { Algorithm } from "./algorithm.ts";
import type { Key } from "./key.ts";

/**
 * Generate a new key for the given algorithm
 */
export async function generate(algorithm: Algorithm, kid?: string): Promise<Key> {
	switch (algorithm) {
		case "HS256":
			return generateHmacKey(algorithm, 32, kid);
		case "HS384":
			return generateHmacKey(algorithm, 48, kid);
		case "HS512":
			return generateHmacKey(algorithm, 64, kid);
		case "RS256":
		case "RS384":
		case "RS512":
			return generateRsaKey(algorithm, "RSASSA-PKCS1-v1_5", kid);
		case "PS256":
		case "PS384":
		case "PS512":
			return generateRsaKey(algorithm, "RSA-PSS", kid);
		case "ES256":
			return generateEcKey(algorithm, "P-256", kid);
		case "ES384":
			return generateEcKey(algorithm, "P-384", kid);
		case "EdDSA":
			return generateEdDsaKey(algorithm, kid);
		default:
			throw new Error(`Unsupported algorithm: ${algorithm}`);
	}
}

/**
 * Generate an HMAC symmetric key
 */
async function generateHmacKey(alg: Algorithm, byteLength: number, kid?: string): Promise<Key> {
	const bytes = new Uint8Array(byteLength);
	crypto.getRandomValues(bytes);

	const k = base64.fromArrayBuffer(bytes.buffer, true);

	return {
		kty: "oct",
		alg,
		k,
		key_ops: ["sign", "verify"],
		...(kid && { kid }),
	};
}

/**
 * Generate an RSA asymmetric key pair
 */
async function generateRsaKey(alg: Algorithm, name: "RSASSA-PKCS1-v1_5" | "RSA-PSS", kid?: string): Promise<Key> {
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

	const jwk = (await crypto.subtle.exportKey("jwk", keyPair.privateKey)) as {
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
		key_ops: ["sign", "verify"],
		...(kid && { kid }),
	};
}

/**
 * Generate an elliptic curve asymmetric key pair
 */
async function generateEcKey(alg: "ES256" | "ES384", namedCurve: "P-256" | "P-384", kid?: string): Promise<Key> {
	const keyPair = await crypto.subtle.generateKey(
		{
			name: "ECDSA",
			namedCurve,
		},
		true,
		["sign", "verify"],
	);

	const jwk = (await crypto.subtle.exportKey("jwk", keyPair.privateKey)) as {
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
		key_ops: ["sign", "verify"],
		...(kid && { kid }),
	};
}

/**
 * Generate an EdDSA key pair using Ed25519
 */
async function generateEdDsaKey(alg: "EdDSA", kid?: string): Promise<Key> {
	const keyPair = await crypto.subtle.generateKey(
		{
			name: "Ed25519",
		},
		true,
		["sign", "verify"],
	);

	const jwk = (await crypto.subtle.exportKey("jwk", keyPair.privateKey)) as {
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
		key_ops: ["sign", "verify"],
		...(kid && { kid }),
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
