import * as base64 from "@hexagon/base64";
import * as jose from "jose";
import { z } from "zod";
import { type Algorithm, AlgorithmSchema } from "./algorithm";
import { type Claims, ClaimsSchema, validateClaims } from "./claims";

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

const Base64FieldSchema = z
	.string()
	.min(1)
	.refine((value) => decodeBase64Flexible(value) !== null, {
		message: "Field must be valid base64url data",
	});

const BaseKeySchema = z.object({
	alg: AlgorithmSchema,
	key_ops: z.array(OperationSchema).nonempty(),
	kid: z.string().optional(),
});

const OctKeySchema = BaseKeySchema.extend({
	kty: z.literal("oct"),
	k: Base64FieldSchema.refine(
		(secret) => {
			// Validate minimum length (at least 32 bytes when decoded)
			const decoded = decodeBase64Flexible(secret);
			return decoded && decoded.byteLength >= MIN_HMAC_SECRET_BYTES;
		},
		{
			message: `Secret must be at least ${MIN_HMAC_SECRET_BYTES} bytes when decoded`,
		},
	),
});

const LegacyOctKeySchema = BaseKeySchema.extend({
	k: Base64FieldSchema,
	kty: z.undefined().optional(),
});

const RsaKeySchema = BaseKeySchema.extend({
	kty: z.literal("RSA"),
	n: Base64FieldSchema,
	e: Base64FieldSchema,
	d: Base64FieldSchema.optional(),
	p: Base64FieldSchema.optional(),
	q: Base64FieldSchema.optional(),
	dp: Base64FieldSchema.optional(),
	dq: Base64FieldSchema.optional(),
	qi: Base64FieldSchema.optional(),
}).superRefine((data, ctx) => {
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
});

const EcKeySchema = BaseKeySchema.extend({
	kty: z.literal("EC"),
	crv: z.enum(["P-256", "P-384"]),
	x: Base64FieldSchema,
	y: Base64FieldSchema,
	d: Base64FieldSchema.optional(),
});

const OkpKeySchema = BaseKeySchema.extend({
	kty: z.literal("OKP"),
	crv: z.literal("Ed25519"),
	x: Base64FieldSchema,
	d: Base64FieldSchema.optional(),
});

const CanonicalKeySchema = z.discriminatedUnion("kty", [OctKeySchema, RsaKeySchema, EcKeySchema, OkpKeySchema]);
export const KeySchema = CanonicalKeySchema;
export type Key = z.infer<typeof KeySchema>;
export type AsymmetricKey = Exclude<Key, { kty: "oct" }>;
export type SymmetricKey = Extract<Key, { kty: "oct" }>;
export type PublicKey = Omit<AsymmetricKey, "d" | "p" | "q" | "dp" | "dq" | "qi">;
type LegacyOctKey = z.infer<typeof LegacyOctKeySchema>;

export function toPublicKey(key: Key): PublicKey {
	switch (key.kty) {
		case "oct":
			throw new Error("Cannot derive public key from oct (symmetric) key");

		case "RSA": {
			// biome-ignore lint/correctness/noUnusedVariables: On purpose to remove fields from publicKey
			const { d, p, q, dp, dq, qi, key_ops, ...publicKey } = key;
			return { ...publicKey, key_ops: key_ops.filter((op) => op !== "sign" && op !== "decrypt") };
		}

		case "EC": {
			// biome-ignore lint/correctness/noUnusedVariables: On purpose to remove fields from publicKey
			const { d, key_ops, ...publicKey } = key;
			return { ...publicKey, key_ops: key_ops.filter((op) => op !== "sign" && op !== "decrypt") };
		}

		case "OKP": {
			// biome-ignore lint/correctness/noUnusedVariables: On purpose to remove fields from publicKey
			const { d, key_ops, ...publicKey } = key;
			return { ...publicKey, key_ops: key_ops.filter((op) => op !== "sign" && op !== "decrypt") };
		}
	}
}

export function load(jwk: string): Key {
	const key = loadKey(jwk);
	if (key.kty !== "oct") {
		ensurePrivateMaterial(key as Key);
	}
	return key as Key;
}

export function loadPublic(jwk: string): PublicKey {
	const key = loadKey(jwk);
	if (key.kty === "oct") {
		throw new Error("Cannot load oct (symmetric) key as a public key; use load() instead.");
	}
	return toPublicKey(key as Key);
}

function loadKey(jwk: string): Key | PublicKey {
	const decoded = decodeBase64Flexible(jwk.trim());
	if (!decoded) {
		throw new Error("Failed to decode JWK: invalid base64url encoding");
	}

	let data: unknown;
	try {
		const jsonString = new TextDecoder().decode(decoded);
		data = JSON.parse(jsonString);
	} catch {
		throw new Error("Failed to parse JWK: invalid JSON format");
	}

	const key = parseKeyWithLegacyFallback(data);

	try {
		validateKey(key);
	} catch (error) {
		throw new Error(`Failed to validate JWK: ${error instanceof Error ? error.message : "unknown error"}`);
	}

	return key;
}

export async function sign(key: Key, claims: Claims): Promise<string> {
	ensureOperationSupported(key, "sign");

	// Validate claims before signing
	try {
		ClaimsSchema.parse(claims);
		validateClaims(claims);
	} catch (error) {
		throw new Error(`Invalid claims: ${error instanceof Error ? error.message : "unknown error"}`);
	}

	const joseKey = await importJoseKey(key);
	const jwt = await new jose.SignJWT(claims)
		.setProtectedHeader({
			alg: key.alg,
			typ: "JWT",
			...(key.kid && { kid: key.kid }),
		})
		.setIssuedAt()
		.sign(joseKey);

	return jwt;
}

export async function verify(key: PublicKey | SymmetricKey, token: string, path: string): Promise<Claims> {
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

	// Validate path matches
	if (claims.root !== path) {
		throw new Error("Token path does not match provided path");
	}

	// Validate claims structure and business rules
	validateClaims(claims);

	return claims;
}

function parseKeyWithLegacyFallback(data: unknown): Key {
	try {
		return KeySchema.parse(data);
	} catch (primaryError) {
		try {
			const legacy = LegacyOctKeySchema.parse(data);
			return upgradeLegacyKey(legacy);
		} catch {
			throw new Error(
				`Failed to validate JWK: ${primaryError instanceof Error ? primaryError.message : "unknown error"}`,
			);
		}
	}
}

function upgradeLegacyKey(key: LegacyOctKey): Key {
	const { kty: _ignored, ...rest } = key;
	return { ...rest, kty: "oct" } as Key;
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
