import * as base64 from "@hexagon/base64";
import * as z from "@zod/mini";
import * as jose from "jose";
import { type Algorithm, AlgorithmSchema } from "./algorithm.ts";
import { type Claims, ClaimsSchema, ScopeSchema, scopeAllows } from "./claims.ts";
import { encodeGrants } from "./wire.ts";

/**
 * A validated key identifier (kid). Only alphanumeric, hyphens, and underscores.
 */
export const KeyIdSchema = z.string().check(
	z.refine((value) => /^[A-Za-z0-9_-]+$/.test(value), {
		message: "Key ID must contain only alphanumeric characters, hyphens, and underscores",
	}),
);
export type KeyId = z.infer<typeof KeyIdSchema>;

/**
 * Key operations that can be performed
 */
export const OperationSchema = z.enum(["sign", "verify", "decrypt", "encrypt"]);
export type Operation = z.infer<typeof OperationSchema>;

const MIN_HMAC_SECRET_BYTES = 32;
const HMAC_ALGORITHMS: ReadonlySet<Algorithm> = new Set(["HS256", "HS384", "HS512"]);
const RSA_ALGORITHMS: ReadonlySet<Algorithm> = new Set(["RS256", "RS384", "RS512", "PS256", "PS384", "PS512"]);
const EC_ALGORITHM_TO_CURVE: Record<"ES256" | "ES384", "P-256" | "P-384"> = {
	ES256: "P-256",
	ES384: "P-384",
};

const Base64FieldSchema = z.string().check(
	z.minLength(1),
	z.refine((value) => decodeBase64Flexible(value) !== null, {
		message: "Field must be valid base64url data",
	}),
);

const BaseKeySchema = z.object({
	alg: AlgorithmSchema,
	// RFC 7517 leaves omitted key_ops unrestricted. Default to the operations this library supports.
	key_ops: z._default(z.array(OperationSchema).check(z.minLength(1)), ["sign", "verify"]),
	kid: z.optional(KeyIdSchema),
	scope: z.optional(ScopeSchema),
});

const OctKeySchema = z.extend(BaseKeySchema, {
	kty: z.literal("oct"),
	k: Base64FieldSchema.check(
		z.refine(
			(secret) => {
				// Validate minimum length (at least 32 bytes when decoded)
				const decoded = decodeBase64Flexible(secret);
				return decoded && decoded.byteLength >= MIN_HMAC_SECRET_BYTES;
			},
			{
				message: `Secret must be at least ${MIN_HMAC_SECRET_BYTES} bytes when decoded`,
			},
		),
	),
});

const RsaKeySchema = z
	.extend(BaseKeySchema, {
		kty: z.literal("RSA"),
		n: Base64FieldSchema,
		e: Base64FieldSchema,
		d: z.optional(Base64FieldSchema),
		p: z.optional(Base64FieldSchema),
		q: z.optional(Base64FieldSchema),
		dp: z.optional(Base64FieldSchema),
		dq: z.optional(Base64FieldSchema),
		qi: z.optional(Base64FieldSchema),
	})
	.check(
		z.superRefine((data, ctx) => {
			// The RFC requires only d, the others are only required as soon as one is present
			// https://datatracker.ietf.org/doc/html/rfc7518#section-6.3.2
			// But WebCrypto requires all parameters to be present for private keys
			const privFields = ["d", "p", "q", "dp", "dq", "qi"] as const;

			const present = privFields.filter((f) => data[f] !== undefined);

			if (present.length > 0 && present.length < privFields.length) {
				ctx.addIssue({
					code: "custom",
					message: "If any private RSA fields are present, all private RSA fields must be present.",
				});
			}
		}),
	);

const EcKeySchema = z.extend(BaseKeySchema, {
	kty: z.literal("EC"),
	crv: z.enum(["P-256", "P-384"]),
	x: Base64FieldSchema,
	y: Base64FieldSchema,
	d: z.optional(Base64FieldSchema),
});

const OkpKeySchema = z.extend(BaseKeySchema, {
	kty: z.literal("OKP"),
	crv: z.literal("Ed25519"),
	x: Base64FieldSchema,
	d: z.optional(Base64FieldSchema),
});

const CanonicalKeySchema = z.discriminatedUnion("kty", [OctKeySchema, RsaKeySchema, EcKeySchema, OkpKeySchema]);
export const KeySchema = CanonicalKeySchema;
export type Key = z.infer<typeof KeySchema>;
export type AsymmetricKey = Exclude<Key, { kty: "oct" }>;
export type SymmetricKey = Extract<Key, { kty: "oct" }>;
export type PublicKey = Omit<AsymmetricKey, "d" | "p" | "q" | "dp" | "dq" | "qi">;

/** Derive a verify-only copy of this key, dropping the private material. */
function toPublic(key: Key): PublicKey {
	switch (key.kty) {
		case "oct":
			throw new Error("Cannot derive public key from oct (symmetric) key");

		case "RSA": {
			const { d, p, q, dp, dq, qi, key_ops, ...publicKey } = key;
			return { ...publicKey, key_ops: key_ops.filter((op) => op !== "sign" && op !== "decrypt") };
		}

		case "EC": {
			const { d, key_ops, ...publicKey } = key;
			return { ...publicKey, key_ops: key_ops.filter((op) => op !== "sign" && op !== "decrypt") };
		}

		case "OKP": {
			const { d, key_ops, ...publicKey } = key;
			return { ...publicKey, key_ops: key_ops.filter((op) => op !== "sign" && op !== "decrypt") };
		}
	}
}

/** Parse a key from a string, auto-detecting JSON or base64url encoding. */
function parse(jwk: string): Key {
	const trimmed = jwk.trim();

	let data: unknown;
	if (trimmed.startsWith("{")) {
		// Plain JSON
		try {
			data = JSON.parse(trimmed);
		} catch {
			throw new Error("Failed to parse JWK: invalid JSON format");
		}
	} else {
		// Base64url encoded JSON
		const decoded = decodeBase64Flexible(trimmed);
		if (!decoded) {
			throw new Error("Failed to decode JWK: invalid base64url encoding");
		}
		try {
			const jsonString = new TextDecoder().decode(decoded);
			data = JSON.parse(jsonString);
		} catch {
			throw new Error("Failed to parse JWK: invalid JSON format after base64url decode");
		}
	}

	let key: Key;
	try {
		key = KeySchema.parse(data);
	} catch (error) {
		throw new Error(`Failed to validate JWK: ${error instanceof Error ? error.message : "unknown error"}`);
	}

	try {
		validateKey(key);
	} catch (error) {
		throw new Error(`Failed to validate JWK: ${error instanceof Error ? error.message : "unknown error"}`);
	}

	return key;
}

/**
 * Sign the claims with this key, returning the encoded token.
 *
 * `iat` is written only when the claims carry one, so an unset field stays off the
 * wire rather than being stamped with the current time.
 */
async function sign(key: Key, claims: Claims): Promise<string> {
	ensureOperationSupported(key, "sign");

	// Validate claims before signing
	try {
		ClaimsSchema.parse(claims);
	} catch (error) {
		throw new Error(`Invalid claims: ${error instanceof Error ? error.message : "unknown error"}`);
	}
	ensureClaimsWithinScope(key, claims);

	const joseKey = await importJoseKey(key);
	// Written the legacy way when that says the same thing, so older verifiers accept it.
	const jwt = await new jose.SignJWT(encodeGrants(claims))
		.setProtectedHeader({
			alg: key.alg,
			typ: "JWT",
			...(key.kid && { kid: key.kid }),
		})
		.sign(joseKey);

	return jwt;
}

/**
 * Verify a token's signature with this key and return its claims.
 *
 * Rejects an expired token (the `exp` claim). Scoping the claims to a connection path
 * is a separate step; see {@link authorize}.
 */
async function verify(key: PublicKey | SymmetricKey, token: string): Promise<Claims> {
	ensureOperationSupported(key, "verify");
	const joseKey = await importJoseKey(key);
	const { payload } = await jose.jwtVerify(token, joseKey, {
		algorithms: [key.alg],
	});

	let claims: Claims;
	try {
		claims = ClaimsSchema.parse(payload);
	} catch (error) {
		throw new Error(`Failed to parse token claims: ${error instanceof Error ? error.message : "unknown error"}`);
	}

	// Re-check on the way in, so a scope cannot be stripped by re-signing elsewhere.
	ensureClaimsWithinScope(key, claims);

	return claims;
}

/** Generate a random key ID (16 hex characters). */
function randomKid(): KeyId {
	const bytes = new Uint8Array(8);
	crypto.getRandomValues(bytes);
	return Array.from(bytes)
		.map((b) => b.toString(16).padStart(2, "0"))
		.join("") as KeyId;
}

/** Generate a new key for the given algorithm. A random key ID is assigned if none is provided. */
async function generate(algorithm: Algorithm, kid?: string): Promise<Key> {
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

function getHashForAlgorithm(alg: Algorithm): "SHA-256" | "SHA-384" | "SHA-512" {
	if (alg.endsWith("256")) return "SHA-256";
	if (alg.endsWith("384")) return "SHA-384";
	if (alg.endsWith("512")) return "SHA-512";
	throw new Error(`Cannot determine hash for algorithm: ${alg}`);
}

/**
 * Parse, generate, sign, and verify a JWK.
 *
 * The counterpart of the Rust `moq-auth` crate's `Key`.
 */
export const Key = {
	/** Parse a key from JSON or base64url-encoded JSON. */
	parse,
	/** Derive a verify-only copy, dropping the private material. Throws on a symmetric (oct) key, which has no public half. */
	public: toPublic,
	/** Sign the claims with this key, returning the encoded token. */
	sign,
	/** Verify a token's signature with this key and return its claims. */
	verify,
	/** Generate a key for the given algorithm. A random key ID is assigned if none is provided. */
	generate,
};

function ensureClaimsWithinScope(key: PublicKey | SymmetricKey | Key, claims: Claims): void {
	if (key.scope && !scopeAllows(key.scope, claims)) {
		throw new Error("Token capabilities exceed the key scope");
	}
}

function validateKey(key: Key): void {
	switch (key.kty) {
		case "oct": {
			if (!HMAC_ALGORITHMS.has(key.alg)) {
				throw new Error(`Algorithm ${key.alg} is incompatible with oct keys`);
			}
			const secret = decodeBase64Flexible(key.k);
			if (!secret || secret.byteLength < MIN_HMAC_SECRET_BYTES) {
				throw new Error("Secret must be at least 32 bytes when decoded");
			}
			break;
		}
		case "RSA": {
			if (!RSA_ALGORITHMS.has(key.alg)) {
				throw new Error(`Algorithm ${key.alg} is incompatible with RSA keys`);
			}
			break;
		}
		case "EC": {
			if (!isEcAlgorithm(key.alg)) {
				throw new Error(`Algorithm ${key.alg} is incompatible with EC keys`);
			}
			const expectedCurve = EC_ALGORITHM_TO_CURVE[key.alg];
			if (key.crv !== expectedCurve) {
				throw new Error(`Algorithm ${key.alg} requires curve ${expectedCurve}`);
			}
			break;
		}
		case "OKP": {
			if (key.alg !== "EdDSA") {
				throw new Error(`Algorithm ${key.alg} is incompatible with OKP keys`);
			}
			if (key.crv !== "Ed25519") {
				throw new Error("Only Ed25519 OKP keys are supported");
			}
			break;
		}
		default:
			throw new Error(`Unsupported key type ${(key as { kty: string }).kty}`);
	}
}

function ensureOperationSupported(key: Key | PublicKey, operation: Operation): void {
	if (!key.key_ops.includes(operation)) {
		throw new Error(`Key does not support ${operation} operation`);
	}

	if (operation === "sign") {
		ensurePrivateMaterial(key as Key);
	}
}

function ensurePrivateMaterial(key: Key): void {
	switch (key.kty) {
		case "oct":
			return; // shared secret already validated by validateKey()
		case "RSA":
			if (!key.d) {
				throw new Error("RSA key is missing the private exponent required for signing");
			}
			return;
		case "EC":
			if (!key.d) {
				throw new Error("EC key is missing the private scalar required for signing");
			}
			return;
		case "OKP":
			if (!key.d) {
				throw new Error("OKP key is missing the private scalar required for signing");
			}
			return;
	}
}

function isEcAlgorithm(alg: Algorithm): alg is "ES256" | "ES384" {
	return alg === "ES256" || alg === "ES384";
}

async function importJoseKey(key: Key | PublicKey): Promise<CryptoKey | Uint8Array> {
	const jwk = { ...key } as jose.JWK;
	delete jwk.key_ops;
	return jose.importJWK(jwk, key.alg);
}

function decodeBase64Flexible(value: string): Uint8Array | null {
	const trimmed = value.trim();
	if (!trimmed) {
		return null;
	}

	try {
		// First decode as URL
		return new Uint8Array(base64.toArrayBuffer(trimmed, true));
	} catch {
		try {
			// Fallback to standard base64
			return new Uint8Array(base64.toArrayBuffer(trimmed, false));
		} catch {
			return null;
		}
	}
}
