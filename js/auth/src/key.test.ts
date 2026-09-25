import { expect, test } from "bun:test";
import * as base64 from "@hexagon/base64";
import { exportJWK, generateKeyPair, SignJWT } from "jose";
import type { Algorithm } from "./algorithm.ts";
import { authorize, type Claims } from "./claims.ts";
import { INSECURE_TEST_RS256_OTHER, INSECURE_TEST_RSA_KEYS } from "./insecure-test-keys.ts";
import { Key } from "./key.ts";

// Helper function to encode JSON to base64url
function encodeJwk(obj: unknown): string {
	const jsonString = JSON.stringify(obj);
	const data = new TextEncoder().encode(jsonString);
	return base64.fromArrayBuffer(data.buffer as ArrayBuffer, true); // true for urlSafe
}

const testKey = {
	alg: "HS256",
	key_ops: ["sign", "verify"],
	kty: "oct",
	k: "dGVzdC1zZWNyZXQtdGhhdC1pcy1sb25nLWVub3VnaC1mb3ItaG1hYy1zaGEyNTY", // "test-secret-that-is-long-enough-for-hmac-sha256" in base64url
	kid: "test-key-1",
} as const;

const testClaims: Claims = {
	root: "test-path",
	publish: ["test-pub/**"],
	subscribe: ["test-sub/**"],
	exp: Math.floor((Date.now() + 60 * 1000) / 1000), // 1 minute from now in seconds
	iat: Math.floor(Date.now() / 1000), // now in seconds
};

type AsymmetricAlgorithm = Exclude<Algorithm, "HS256" | "HS384" | "HS512">;

async function generateAsymmetricKeyPair(
	alg: AsymmetricAlgorithm,
): Promise<{ privateEncoded: string; publicEncoded: string }> {
	const { privateKey, publicKey } = await generateKeyPair(alg, { extractable: true });
	const privateJwk = await exportJWK(privateKey);
	const publicJwk = await exportJWK(publicKey);

	return {
		privateEncoded: encodeJwk({
			...privateJwk,
			alg,
			key_ops: ["sign", "verify"],
			kid: `test-${alg}`,
		}),
		publicEncoded: encodeJwk({
			...publicJwk,
			alg,
			key_ops: ["verify"],
			kid: `test-${alg}`,
		}),
	};
}

test("parse - valid JWK", () => {
	const jwk = encodeJwk(testKey);
	const key = Key.parse(jwk);

	expect(key.alg).toBe("HS256");
	expect(key.key_ops).toEqual(["sign", "verify"]);
	expect(key.kty).toBe("oct");
	expect((key as { k?: string }).k).toBe(testKey.k);
	expect(key.kid).toBe("test-key-1");
});

test("parse - invalid base64url", () => {
	const invalidJwk = "invalid-base64url!@#$%";

	expect(() => {
		Key.parse(invalidJwk);
	}).toThrow();
});

test("parse - invalid JSON after base64url decode", () => {
	// Base64url encode invalid JSON
	const data = new TextEncoder().encode("invalid json");
	const invalidJwk = base64.fromArrayBuffer(data.buffer as ArrayBuffer, true); // true for urlSafe

	expect(() => {
		Key.parse(invalidJwk);
	}).toThrow();
});

test("parse - invalid secret format", () => {
	const invalidKey = {
		...testKey,
		k: "invalid-base64url-chars!@#$%",
	};
	const jwk = encodeJwk(invalidKey);

	expect(() => {
		Key.parse(jwk);
	}).toThrow();
});

test("parse - secret too short", () => {
	const invalidKey = {
		...testKey,
		k: "c2hvcnQ", // "short" in base64url (only 5 bytes)
	};
	const jwk = encodeJwk(invalidKey);

	expect(() => {
		Key.parse(jwk);
	}).toThrow();
});

test("parse - missing required fields", () => {
	const invalidKey = {
		alg: "HS256",
		kty: "oct",
		// missing k
	};
	const jwk = encodeJwk(invalidKey);

	expect(() => {
		Key.parse(jwk);
	}).toThrow();
});

test("parse - missing key_ops defaults to sign and verify", async () => {
	const { key_ops: _ignored, ...keyWithoutOps } = testKey;
	const key = Key.parse(encodeJwk(keyWithoutOps));

	expect(key.key_ops).toEqual(["sign", "verify"]);

	const token = await Key.sign(key, testClaims);
	const claims = await Key.verify(key, token);
	expect(claims.root).toBe(testClaims.root);
});

test("public - defaulted key_ops yields exactly verify", async () => {
	const { privateKey } = await generateKeyPair("EdDSA", { extractable: true });
	const jwk = await exportJWK(privateKey);
	const key = Key.parse(encodeJwk({ ...jwk, alg: "EdDSA", kid: "defaulted" }));
	expect(key.key_ops).toEqual(["sign", "verify"]);

	const publicKey = Key.public(key);
	expect(publicKey.key_ops).toEqual(["verify"]);
});

test("parse - oct key without kty is refused", () => {
	const { kty: _ignored, ...legacyKey } = testKey;
	const jwk = encodeJwk(legacyKey);

	expect(() => {
		Key.parse(jwk);
	}).toThrow(/Failed to validate JWK/);
});

test("sign - successful signing", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const token = await Key.sign(key, testClaims);

	expect(typeof token === "string").toBeTruthy();
	expect(token.length > 0).toBeTruthy();
	expect(token.split(".").length === 3).toBeTruthy(); // JWT format: header.payload.signature
});

test("sign - key doesn't support signing", async () => {
	const verifyOnlyKey = {
		...testKey,
		key_ops: ["verify"],
	};
	const key = Key.parse(encodeJwk(verifyOnlyKey));

	await expect(
		(async () => {
			await Key.sign(key, testClaims);
		})(),
	).rejects.toThrow();
});

test("verify - successful verification", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const token = await Key.sign(key, testClaims);
	const claims = await Key.verify(key, token);

	expect(claims.root).toBe(testClaims.root);
	expect(claims.publish).toEqual(testClaims.publish);
	expect(claims.subscribe).toEqual(testClaims.subscribe);
});

test("verify - key doesn't support verification", async () => {
	const signOnlyKey = {
		...testKey,
		key_ops: ["sign"],
	};
	const key = Key.parse(encodeJwk(signOnlyKey));

	await expect(
		(async () => {
			await Key.verify(key, "some.jwt.token");
		})(),
	).rejects.toThrow();
});

test("verify - invalid token format", async () => {
	const key = Key.parse(encodeJwk(testKey));

	await expect(
		(async () => {
			await Key.verify(key, "invalid-token");
		})(),
	).rejects.toThrow();
});

test("verify - expired token", async () => {
	const expiredClaims: Claims = {
		...testClaims,
		exp: Math.floor((Date.now() - 60 * 1000) / 1000), // 1 minute ago in seconds
	};

	const key = Key.parse(encodeJwk(testKey));
	const token = await Key.sign(key, expiredClaims);

	await expect(
		(async () => {
			await Key.verify(key, token);
		})(),
	).rejects.toThrow();
});

test("verify - token without exp field", async () => {
	const claimsWithoutExp: Claims = {
		root: "test-path",
		publish: ["test-pub/**"],
	};

	const key = Key.parse(encodeJwk(testKey));
	const token = await Key.sign(key, claimsWithoutExp);
	const claims = await Key.verify(key, token);

	expect(claims.root).toBe("test-path");
	expect(claims.publish).toEqual(["test-pub/**"]);
	expect(claims.exp).toBe(undefined);
});

test("claims validation - must have pub or sub", async () => {
	const invalidClaims = {
		root: "test-path",
		// missing both pub and sub
	};

	const key = Key.parse(encodeJwk(testKey));

	await expect(
		(async () => {
			await Key.sign(key, invalidClaims as Claims);
		})(),
	).rejects.toThrow();
});

test("round-trip - sign and verify", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const originalClaims: Claims = {
		root: "test-path",
		publish: ["test-pub/**"],
		subscribe: ["test-sub/**"],
		exp: Math.floor((Date.now() + 60 * 1000) / 1000),
		iat: Math.floor(Date.now() / 1000),
	};

	const token = await Key.sign(key, originalClaims);
	const verifiedClaims = await Key.verify(key, token);

	expect(verifiedClaims.root).toBe(originalClaims.root);
	expect(verifiedClaims.publish).toEqual(originalClaims.publish);
	expect(verifiedClaims.subscribe).toEqual(originalClaims.subscribe);
	expect(verifiedClaims.exp).toBe(originalClaims.exp);
	expect(verifiedClaims.iat).toBe(originalClaims.iat);
});

test("verify - ignores the path, which authorize() checks instead", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const token = await Key.sign(key, testClaims);

	// Verification is signature-only, so a token for an unrelated path still decodes...
	const claims = await Key.verify(key, token);
	expect(claims.root).toBe(testClaims.root);

	// ...and is rejected only once authorized against that path.
	expect(() => authorize(claims, "different-path")).toThrow();
});

test("sign - invalid claims without pub or sub", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const invalidClaims = {
		root: "test-path",
	};

	await expect(
		(async () => {
			await Key.sign(key, invalidClaims as Claims);
		})(),
	).rejects.toThrow();
});

test("sign - claims validation path not prefix absolute sub", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const validClaims: Claims = {
		root: "test-path",
		subscribe: ["absolute-sub/**"],
	};

	const token = await Key.sign(key, validClaims);
	expect(typeof token === "string").toBeTruthy();
	expect(token.length > 0).toBeTruthy();
});

test("sign - claims validation path is prefix with relative paths", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const validClaims: Claims = {
		root: "test-path",
		publish: ["relative-pub/**"],
		subscribe: ["relative-sub/**"],
	};

	const token = await Key.sign(key, validClaims);
	expect(typeof token === "string").toBeTruthy();
	expect(token.length > 0).toBeTruthy();
});

test("sign - claims validation empty root", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const validClaims: Claims = {
		root: "",
		publish: ["test-pub/**"],
	};

	const token = await Key.sign(key, validClaims);
	expect(typeof token === "string").toBeTruthy();
	expect(token.length > 0).toBeTruthy();
});

test("different algorithms - HS384", async () => {
	const hs384Key = {
		alg: "HS384",
		key_ops: ["sign", "verify"],
		kty: "oct",
		k: "dGVzdC1zZWNyZXQtdGhhdC1pcy1sb25nLWVub3VnaC1mb3ItaG1hYy1zaGEzODQtYWxnb3JpdGhtLXRlc3RpbmctcHVycG9zZXM", // longer secret for HS384
		kid: "test-key-hs384",
	} as const;

	const key = Key.parse(encodeJwk(hs384Key));
	const token = await Key.sign(key, testClaims);
	const verifiedClaims = await Key.verify(key, token);

	expect(verifiedClaims.root).toBe(testClaims.root);
	expect(verifiedClaims.publish).toEqual(testClaims.publish);
});

test("different algorithms - HS512", async () => {
	const hs512Key = {
		alg: "HS512",
		key_ops: ["sign", "verify"],
		kty: "oct",
		k: "dGVzdC1zZWNyZXQtdGhhdC1pcy1sb25nLWVub3VnaC1mb3ItaG1hYy1zaGE1MTItYWxnb3JpdGhtLXRlc3RpbmctcHVycG9zZXMtYW5kLW1vcmUtZGF0YQ", // longer secret for HS512
		kid: "test-key-hs512",
	} as const;

	const key = Key.parse(encodeJwk(hs512Key));
	const token = await Key.sign(key, testClaims);
	const verifiedClaims = await Key.verify(key, token);

	expect(verifiedClaims.root).toBe(testClaims.root);
	expect(verifiedClaims.publish).toEqual(testClaims.publish);
});

test("verify - private to public key", async () => {
	const key = Key.parse(INSECURE_TEST_RSA_KEYS.RS256.private);
	const publicKey = Key.public(key);

	expect(publicKey.alg).toBe(key.alg);

	expect(key.key_ops.indexOf("sign") >= 0).toBeTruthy();
	expect(key.key_ops.indexOf("verify") >= 0).toBeTruthy();
	expect(publicKey.key_ops.indexOf("sign") === -1).toBeTruthy();
	expect(publicKey.key_ops.indexOf("verify") >= 0).toBeTruthy();
});

test("RSA algorithms - sign and verify", async () => {
	for (const alg of ["RS256", "RS384", "RS512"] as const) {
		const key = Key.parse(INSECURE_TEST_RSA_KEYS[alg].private);
		expect(key.alg).toBe(alg);

		const token = await Key.sign(key, testClaims);
		const verifiedClaims = await Key.verify(Key.public(key), token);
		expect(verifiedClaims.root).toBe(testClaims.root);
	}
});

test("RSA public keys verify but cannot sign", async () => {
	const privateKey = Key.parse(INSECURE_TEST_RSA_KEYS.RS256.private);
	const publicKey = Key.parse(INSECURE_TEST_RSA_KEYS.RS256.public);

	const token = await Key.sign(privateKey, testClaims);
	const claims = await Key.verify(publicKey, token);
	expect(claims.root).toBe(testClaims.root);

	await expect(
		(async () => {
			await Key.sign(publicKey as Key, testClaims);
		})(),
	).rejects.toThrow();
});

test("RSA-PSS algorithms - sign and verify", async () => {
	for (const alg of ["PS256", "PS384", "PS512"] as const) {
		const key = Key.parse(INSECURE_TEST_RSA_KEYS[alg].private);
		expect(key.alg).toBe(alg);

		const token = await Key.sign(key, testClaims);
		const verifiedClaims = await Key.verify(Key.public(key), token);
		expect(verifiedClaims.root).toBe(testClaims.root);
	}
});

test("RSA verification rejects foreign keys and mangled tokens", async () => {
	const key = Key.parse(INSECURE_TEST_RSA_KEYS.RS256.private);
	const token = await Key.sign(key, testClaims);

	// A different RS256 key is the same algorithm and the same shape, so only the
	// signature check can reject it.
	const foreign = Key.parse(INSECURE_TEST_RS256_OTHER.public);
	await expect(Key.verify(foreign, token)).rejects.toThrow();

	const publicKey = Key.parse(INSECURE_TEST_RSA_KEYS.RS256.public);
	const [header, payload, signature] = token.split(".");

	// Truncated, over-segmented, and empty tokens are all malformed.
	await expect(Key.verify(publicKey, `${header}.${payload}`)).rejects.toThrow();
	await expect(Key.verify(publicKey, `${token}.${signature}`)).rejects.toThrow();
	await expect(Key.verify(publicKey, "")).rejects.toThrow();

	// So is a well-formed token whose signature no longer covers the payload.
	const tamperedPayload = encodeJwk({ ...testClaims, root: "somewhere-else" });
	await expect(Key.verify(publicKey, `${header}.${tamperedPayload}.${signature}`)).rejects.toThrow();
	await expect(Key.verify(publicKey, `${header}.${payload}.${signature.slice(0, -4)}AAAA`)).rejects.toThrow();
});

// RSA key generation searches for random primes, so its cost has a long tail:
// unlike signing, it has no bound. Generating a pair per algorithm inline is
// what pushed the sign/verify tests above past Bun's 5s per-test deadline under
// concurrent suite load, so those use committed fixtures and generation is
// covered exactly once, here, where it is the behavior under test. The deadline
// below is local to this test rather than a suite-wide bump, and is roughly ten
// times the slowest single generation measured under load.
test("RSA key generation - a generated key signs and verifies", async () => {
	const key = await Key.generate("RS256", "generated");
	expect(key.alg).toBe("RS256");
	expect(key.kty).toBe("RSA");

	const token = await Key.sign(key, testClaims);
	const claims = await Key.verify(Key.public(key), token);
	expect(claims.root).toBe(testClaims.root);

	// A fresh key is genuinely fresh: it cannot verify a fixture's signature.
	const fixtureToken = await Key.sign(Key.parse(INSECURE_TEST_RSA_KEYS.RS256.private), testClaims);
	await expect(Key.verify(Key.public(key), fixtureToken)).rejects.toThrow();
}, 30_000);

test("EC algorithms - sign and verify", async () => {
	for (const alg of ["ES256", "ES384"] as const) {
		const { privateEncoded } = await generateAsymmetricKeyPair(alg);
		const key = Key.parse(privateEncoded);
		const token = await Key.sign(key, testClaims);
		const verifiedClaims = await Key.verify(Key.public(key), token);
		expect(verifiedClaims.root).toBe(testClaims.root);
	}
});

test("EdDSA algorithm - sign and verify", async () => {
	const { privateEncoded, publicEncoded } = await generateAsymmetricKeyPair("EdDSA");
	const privateKey = Key.parse(privateEncoded);
	const publicKey = Key.parse(publicEncoded);
	const token = await Key.sign(privateKey, testClaims);
	const verifiedClaims = await Key.verify(publicKey, token);
	expect(verifiedClaims.root).toBe(testClaims.root);
});

test("EdDSA algorithm - static sign and verify", async () => {
	// Key generated via `moq auth generate`
	const privateKey =
		"eyJhbGciOiJFZERTQSIsImtleV9vcHMiOlsidmVyaWZ5Iiwic2lnbiJdLCJrdHkiOiJPS1AiLCJjcnYiOiJFZDI1NTE5IiwieCI6Imd4cXVxMDlJUE4xVHl1TG1nTnNqZmo2NWtoa05OWndKVmp1MEEtUmQ0dkEiLCJkIjoiU1NFSHBIeTFUNHJaemhua3dpVVFlUGV1TUh2MWpLUGlxRzRsbFhyQV91cyJ9";
	const key = Key.parse(privateKey);
	const token = await Key.sign(key, testClaims);
	const verifiedClaims = await Key.verify(Key.public(key), token);
	expect(verifiedClaims.root).toBe(testClaims.root);
});

test("EdDSA algorithm - verify with private key fails", async () => {
	// Key generated via `moq auth generate`
	const privateKey =
		"eyJhbGciOiJFZERTQSIsImtleV9vcHMiOlsidmVyaWZ5Iiwic2lnbiJdLCJrdHkiOiJPS1AiLCJjcnYiOiJFZDI1NTE5IiwieCI6Imd4cXVxMDlJUE4xVHl1TG1nTnNqZmo2NWtoa05OWndKVmp1MEEtUmQ0dkEiLCJkIjoiU1NFSHBIeTFUNHJaemhua3dpVVFlUGV1TUh2MWpLUGlxRzRsbFhyQV91cyJ9";
	const key = Key.parse(privateKey);
	const token = await Key.sign(key, testClaims);
	await expect(
		(async () => {
			await Key.verify(key, token);
		})(),
	).rejects.toThrow();
});

test("asymmetric cross-algorithm verification fails", async () => {
	const rsKey = Key.parse(INSECURE_TEST_RSA_KEYS.RS256.private);
	const rsPublicKey = Key.parse(INSECURE_TEST_RSA_KEYS.RS256.public);
	const psPublicKey = Key.parse(INSECURE_TEST_RSA_KEYS.PS256.public);
	const token = await Key.sign(rsKey, testClaims);

	const verifiedClaims = await Key.verify(rsPublicKey, token);
	expect(verifiedClaims.root).toBe(testClaims.root);

	await expect(
		(async () => {
			await Key.verify(psPublicKey, token);
		})(),
	).rejects.toThrow();
});

test("cross-algorithm verification fails", async () => {
	const hs256Key = Key.parse(encodeJwk(testKey));
	const hs384Key = Key.parse(
		encodeJwk({
			alg: "HS384",
			key_ops: ["sign", "verify"],
			kty: "oct",
			k: "dGVzdC1zZWNyZXQtdGhhdC1pcy1sb25nLWVub3VnaC1mb3ItaG1hYy1zaGEzODQtYWxnb3JpdGhtLXRlc3RpbmctcHVycG9zZXM",
			kid: "test-key-hs384",
		}),
	);

	const token = await Key.sign(hs256Key, testClaims);

	await expect(
		(async () => {
			await Key.verify(hs384Key, token);
		})(),
	).rejects.toThrow();
});

test("parse - invalid algorithm", () => {
	const invalidKey = {
		...testKey,
		alg: "ES512", // unsupported algorithm
	};
	const jwk = encodeJwk(invalidKey);

	expect(() => {
		Key.parse(jwk);
	}).toThrow();
});

test("parse - mismatched algorithm", () => {
	const invalidKey = {
		...testKey,
		alg: "RS256", // mismatched algorithm for oct key
	};
	const jwk = encodeJwk(invalidKey);

	expect(() => {
		Key.parse(jwk);
	}).toThrow();
});

test("parse - invalid key_ops", () => {
	const invalidKey = {
		...testKey,
		key_ops: ["invalid-operation"],
	};
	const jwk = encodeJwk(invalidKey);

	expect(() => {
		Key.parse(jwk);
	}).toThrow();
});

test("parse - missing alg field", () => {
	const invalidKey = {
		key_ops: ["sign", "verify"],
		kty: "oct",
		k: testKey.k,
	};
	const jwk = encodeJwk(invalidKey);

	expect(() => {
		Key.parse(jwk);
	}).toThrow();
});

test("sign - includes kid in header when present", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const token = await Key.sign(key, testClaims);

	// Decode the header to verify kid is present
	const [headerB64] = token.split(".");
	const headerBuffer = base64.toArrayBuffer(headerB64, true); // true for urlSafe
	const header = JSON.parse(new TextDecoder().decode(headerBuffer));

	expect(header.kid).toBe("test-key-1");
	expect(header.alg).toBe("HS256");
	expect(header.typ).toBe("JWT");
});

test("sign - no kid in header when not present", async () => {
	const keyWithoutKid = {
		...testKey,
		kid: undefined,
	};
	delete keyWithoutKid.kid;

	const key = Key.parse(encodeJwk(keyWithoutKid));
	const token = await Key.sign(key, testClaims);

	// Decode the header to verify kid is not present
	const [headerB64] = token.split(".");
	const headerBuffer = base64.toArrayBuffer(headerB64, true); // true for urlSafe
	const header = JSON.parse(new TextDecoder().decode(headerBuffer));

	expect(header.kid).toBe(undefined);
	expect(header.alg).toBe("HS256");
	expect(header.typ).toBe("JWT");
});

test("sign - writes iat only when the claims carry one", async () => {
	const key = Key.parse(encodeJwk(testKey));

	const decodePayload = (token: string) => {
		const [, payloadB64] = token.split(".");
		const payloadBuffer = base64.toArrayBuffer(payloadB64, true); // true for urlSafe
		return JSON.parse(new TextDecoder().decode(payloadBuffer));
	};

	// Unset stays off the wire, matching the Rust crate rather than stamping now().
	const withoutIat = await Key.sign(key, { root: "test-path", publish: ["test-pub/**"] });
	expect(decodePayload(withoutIat).iat).toBeUndefined();

	// A caller-supplied iat is preserved exactly.
	const withIat = await Key.sign(key, { root: "test-path", publish: ["test-pub/**"], iat: 1700000000 });
	expect(decodePayload(withIat).iat).toBe(1700000000);
});

test("verify - malformed token parts", async () => {
	const key = Key.parse(encodeJwk(testKey));

	await expect(
		(async () => {
			await Key.verify(key, "invalid");
		})(),
	).rejects.toThrow();

	await expect(
		(async () => {
			await Key.verify(key, "invalid.token");
		})(),
	).rejects.toThrow();

	await expect(
		(async () => {
			await Key.verify(key, "invalid.token.signature.extra");
		})(),
	).rejects.toThrow();
});

test("verify - invalid payload structure", async () => {
	const key = Key.parse(encodeJwk(testKey));

	// Create a token with invalid payload structure
	const headerData = new TextEncoder().encode(JSON.stringify({ alg: "HS256", typ: "JWT" }));
	const header = base64.fromArrayBuffer(headerData.buffer as ArrayBuffer, true); // true for urlSafe

	const payloadData = new TextEncoder().encode(JSON.stringify({ invalid: "payload" }));
	const payload = base64.fromArrayBuffer(payloadData.buffer as ArrayBuffer, true); // true for urlSafe
	const signature = "invalid";
	const invalidToken = `${header}.${payload}.${signature}`;

	await expect(
		(async () => {
			await Key.verify(key, invalidToken);
		})(),
	).rejects.toThrow();
});

test("verify - claims validation during verification", async () => {
	const key = Key.parse(encodeJwk(testKey));

	// We need to create a token with valid claims since Key.sign() would reject invalid ones
	const token = await Key.sign(key, { root: "test-path", publish: ["absolute-pub/**"] });

	// Test that valid tokens pass verification
	const verifiedClaims = await Key.verify(key, token);
	expect(verifiedClaims.root).toBe("test-path");
});

test("verify - a legacy put/get prefix token reads as subtrees", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const secret = await crypto.subtle.importKey(
		"raw",
		Buffer.from(testKey.k, "base64url"),
		{ name: "HMAC", hash: "SHA-256" },
		false,
		["sign"],
	);
	// A well-signed token that speaks prefixes, as @moq/token minted them.
	const legacy = await new SignJWT({ root: "test-path", put: ["alice"], get: [""] })
		.setProtectedHeader({ alg: "HS256", kid: testKey.kid })
		.sign(secret);
	const claims = await Key.verify(key, legacy);
	expect(claims.publish).toEqual(["alice/**"]);
	expect(claims.subscribe).toEqual(["**"]);
});

test("sign - subtree grants are written as legacy put/get", async () => {
	const key = Key.parse(encodeJwk(testKey));
	const token = await Key.sign(key, { root: "test-path", publish: ["alice/**"], subscribe: ["**"] });
	const payload = JSON.parse(Buffer.from(token.split(".")[1], "base64url").toString());
	expect(payload).toEqual({ root: "test-path", put: ["alice"], get: [""] });
});

test("sign - legacy-shaped input cannot slip past a key scope", async () => {
	const scoped: Key = {
		...Key.parse(encodeJwk(testKey)),
		scope: { root: "demo", publish: ["inside/**"] },
	};
	const legacy = { root: "demo", put: ["outside"] } as unknown as Parameters<typeof Key.sign>[1];
	await expect(Key.sign(scoped, legacy)).rejects.toThrow(/scope/);
});

test("key scope is enforced when signing and verifying", async () => {
	const unrestricted = Key.parse(encodeJwk(testKey));
	const scoped: Key = {
		...unrestricted,
		scope: { root: "project", publish: ["live/**"] },
	};
	const allowed: Claims = { root: "project", publish: ["live/room/**"] };
	const denied: Claims = { root: "project", publish: ["other/**"] };

	await expect(Key.sign(scoped, allowed)).resolves.toBeString();
	await expect(Key.sign(scoped, denied)).rejects.toThrow("exceed the key scope");

	// A token signed by an unscoped copy of the key must still be rejected on the
	// way in, so a scope can't be stripped by re-signing elsewhere.
	const forged = await Key.sign(unrestricted, denied);
	await expect(Key.verify(scoped, forged)).rejects.toThrow("exceed the key scope");
});

test("key scope treats absolute and rooted grants alike", async () => {
	const scoped: Key = { ...Key.parse(encodeJwk(testKey)), scope: { root: "project", publish: ["live/**"] } };

	await expect(Key.sign(scoped, { root: "", publish: ["project/live/room/**"] })).resolves.toBeString();
	await expect(Key.sign(scoped, { root: "/project/live/", publish: ["room/**"] })).resolves.toBeString();
	// Segment-aware: "live" must not cover "lively".
	await expect(Key.sign(scoped, { root: "project", publish: ["lively/**"] })).rejects.toThrow("exceed the key scope");
	// A root above the scope does not widen it.
	await expect(Key.sign(scoped, { root: "", publish: ["**"] })).rejects.toThrow("exceed the key scope");
	// Roles are independent: a publish-only scope grants no subscribe.
	await expect(Key.sign(scoped, { root: "project", subscribe: ["live/**"] })).rejects.toThrow("exceed the key scope");
});
