import assert from "node:assert";
import test from "node:test";
import * as base64 from "@hexagon/base64";
import { exportJWK, generateKeyPair } from "jose";
import type { Algorithm } from "./algorithm";
import type { Claims } from "./claims";
import { type Key, load, loadPublic, sign, toPublicKey, verify } from "./key";

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
	put: "test-pub",
	get: "test-sub",
	cluster: false,
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

test("load - valid JWK", () => {
	const jwk = encodeJwk(testKey);
	const key = load(jwk);

	assert.strictEqual(key.alg, "HS256");
	assert.deepEqual(key.key_ops, ["sign", "verify"]);
	assert.strictEqual(key.kty, "oct");
	assert.strictEqual(key.k, testKey.k);
	assert.strictEqual(key.kid, "test-key-1");
});

test("load - invalid base64url", () => {
	const invalidJwk = "invalid-base64url!@#$%";

	assert.throws(() => {
		load(invalidJwk);
	});
});

test("load - invalid JSON after base64url decode", () => {
	// Base64url encode invalid JSON
	const data = new TextEncoder().encode("invalid json");
	const invalidJwk = base64.fromArrayBuffer(data.buffer as ArrayBuffer, true); // true for urlSafe

	assert.throws(() => {
		load(invalidJwk);
	});
});

test("load - invalid secret format", () => {
	const invalidKey = {
		...testKey,
		k: "invalid-base64url-chars!@#$%",
	};
	const jwk = encodeJwk(invalidKey);

	assert.throws(() => {
		load(jwk);
	});
});

test("load - secret too short", () => {
	const invalidKey = {
		...testKey,
		k: "c2hvcnQ", // "short" in base64url (only 5 bytes)
	};
	const jwk = encodeJwk(invalidKey);

	assert.throws(() => {
		load(jwk);
	});
});

test("load - missing required fields", () => {
	const invalidKey = {
		alg: "HS256",
		kty: "oct",
		// missing key_ops and k
	};
	const jwk = encodeJwk(invalidKey);

	assert.throws(() => {
		load(jwk);
	});
});

test("load - legacy oct key without kty", () => {
	const { kty: _ignored, ...legacyKey } = testKey;
	const jwk = encodeJwk(legacyKey);
	const key = load(jwk);

	assert.strictEqual(key.kty, "oct");
	assert.strictEqual(key.alg, testKey.alg);
	assert.deepEqual(key.key_ops, testKey.key_ops);
});

test("sign - successful signing", async () => {
	const key = load(encodeJwk(testKey));
	const token = await sign(key, testClaims);

	assert.ok(typeof token === "string");
	assert.ok(token.length > 0);
	assert.ok(token.split(".").length === 3); // JWT format: header.payload.signature
});

test("sign - key doesn't support signing", async () => {
	const verifyOnlyKey = {
		...testKey,
		key_ops: ["verify"],
	};
	const key = load(encodeJwk(verifyOnlyKey));

	await assert.rejects(async () => {
		await sign(key, testClaims);
	});
});

test("verify - successful verification", async () => {
	const key = load(encodeJwk(testKey));
	const token = await sign(key, testClaims);
	const claims = await verify(key, token, testClaims.root);

	assert.strictEqual(claims.root, testClaims.root);
	assert.strictEqual(claims.put, testClaims.put);
	assert.strictEqual(claims.get, testClaims.get);
	assert.strictEqual(claims.cluster, testClaims.cluster);
});

test("verify - key doesn't support verification", async () => {
	const signOnlyKey = {
		...testKey,
		key_ops: ["sign"],
	};
	const key = load(encodeJwk(signOnlyKey));

	await assert.rejects(async () => {
		await verify(key, "some.jwt.token", "test-path");
	});
});

test("verify - invalid token format", async () => {
	const key = load(encodeJwk(testKey));

	await assert.rejects(async () => {
		await verify(key, "invalid-token", "test-path");
	});
});

test("verify - expired token", async () => {
	const expiredClaims: Claims = {
		...testClaims,
		exp: Math.floor((Date.now() - 60 * 1000) / 1000), // 1 minute ago in seconds
	};

	const key = load(encodeJwk(testKey));
	const token = await sign(key, expiredClaims);

	await assert.rejects(async () => {
		await verify(key, token, expiredClaims.root);
	});
});

test("verify - token without exp field", async () => {
	const claimsWithoutExp: Claims = {
		root: "test-path",
		put: "test-pub",
	};

	const key = load(encodeJwk(testKey));
	const token = await sign(key, claimsWithoutExp);
	const claims = await verify(key, token, claimsWithoutExp.root);

	assert.strictEqual(claims.root, "test-path");
	assert.strictEqual(claims.put, "test-pub");
	assert.strictEqual(claims.exp, undefined);
});

test("claims validation - must have pub or sub", async () => {
	const invalidClaims = {
		root: "test-path",
		cluster: false,
		// missing both pub and sub
	};

	const key = load(encodeJwk(testKey));

	await assert.rejects(async () => {
		await sign(key, invalidClaims as Claims);
	});
});

test("round-trip - sign and verify", async () => {
	const key = load(encodeJwk(testKey));
	const originalClaims: Claims = {
		root: "test-path",
		put: "test-pub",
		get: "test-sub",
		cluster: true,
		exp: Math.floor((Date.now() + 60 * 1000) / 1000),
		iat: Math.floor(Date.now() / 1000),
	};

	const token = await sign(key, originalClaims);
	const verifiedClaims = await verify(key, token, originalClaims.root);

	assert.strictEqual(verifiedClaims.root, originalClaims.root);
	assert.strictEqual(verifiedClaims.put, originalClaims.put);
	assert.strictEqual(verifiedClaims.get, originalClaims.get);
	assert.strictEqual(verifiedClaims.cluster, originalClaims.cluster);
	assert.strictEqual(verifiedClaims.exp, originalClaims.exp);
	assert.strictEqual(verifiedClaims.iat, originalClaims.iat);
});

test("verify - path mismatch", async () => {
	const key = load(encodeJwk(testKey));
	const token = await sign(key, testClaims);

	await assert.rejects(async () => {
		await verify(key, token, "different-path");
	});
});

test("sign - invalid claims without pub or sub", async () => {
	const key = load(encodeJwk(testKey));
	const invalidClaims = {
		root: "test-path",
		cluster: false,
	};

	await assert.rejects(async () => {
		await sign(key, invalidClaims as Claims);
	});
});

test("sign - claims validation path not prefix absolute sub", async () => {
	const key = load(encodeJwk(testKey));
	const validClaims: Claims = {
		root: "test-path",
		get: "absolute-sub",
	};

	const token = await sign(key, validClaims);
	assert.ok(typeof token === "string");
	assert.ok(token.length > 0);
});

test("sign - claims validation path is prefix with relative paths", async () => {
	const key = load(encodeJwk(testKey));
	const validClaims: Claims = {
		root: "test-path",
		put: "relative-pub",
		get: "relative-sub",
	};

	const token = await sign(key, validClaims);
	assert.ok(typeof token === "string");
	assert.ok(token.length > 0);
});

test("sign - claims validation empty root", async () => {
	const key = load(encodeJwk(testKey));
	const validClaims: Claims = {
		root: "",
		put: "test-pub",
	};

	const token = await sign(key, validClaims);
	assert.ok(typeof token === "string");
	assert.ok(token.length > 0);
});

test("different algorithms - HS384", async () => {
	const hs384Key = {
		alg: "HS384",
		key_ops: ["sign", "verify"],
		kty: "oct",
		k: "dGVzdC1zZWNyZXQtdGhhdC1pcy1sb25nLWVub3VnaC1mb3ItaG1hYy1zaGEzODQtYWxnb3JpdGhtLXRlc3RpbmctcHVycG9zZXM", // longer secret for HS384
		kid: "test-key-hs384",
	} as const;

	const key = load(encodeJwk(hs384Key));
	const token = await sign(key, testClaims);
	const verifiedClaims = await verify(key, token, testClaims.root);

	assert.strictEqual(verifiedClaims.root, testClaims.root);
	assert.strictEqual(verifiedClaims.put, testClaims.put);
});

test("different algorithms - HS512", async () => {
	const hs512Key = {
		alg: "HS512",
		key_ops: ["sign", "verify"],
		kty: "oct",
		k: "dGVzdC1zZWNyZXQtdGhhdC1pcy1sb25nLWVub3VnaC1mb3ItaG1hYy1zaGE1MTItYWxnb3JpdGhtLXRlc3RpbmctcHVycG9zZXMtYW5kLW1vcmUtZGF0YQ", // longer secret for HS512
		kid: "test-key-hs512",
	} as const;

	const key = load(encodeJwk(hs512Key));
	const token = await sign(key, testClaims);
	const verifiedClaims = await verify(key, token, testClaims.root);

	assert.strictEqual(verifiedClaims.root, testClaims.root);
	assert.strictEqual(verifiedClaims.put, testClaims.put);
});

test("verify - private to public key", async () => {
	const { privateEncoded } = await generateAsymmetricKeyPair("RS256");
	const key = load(privateEncoded);
	const publicKey = toPublicKey(key);

	assert.equal(publicKey.alg, key.alg);

	assert.ok(key.key_ops.indexOf("sign") >= 0);
	assert.ok(key.key_ops.indexOf("verify") >= 0);
	assert.ok(publicKey.key_ops.indexOf("sign") === -1);
	assert.ok(publicKey.key_ops.indexOf("verify") >= 0);
});

test("RS256 algorithm - static sign and verify", async () => {
	// Key generated via rs/moq-token
	const privateKey =
		"eyJhbGciOiJSUzI1NiIsImtleV9vcHMiOlsidmVyaWZ5Iiwic2lnbiJdLCJrdHkiOiJSU0EiLCJuIjoiMmstRjBzbmtpLTBCYjF5VkVSaUNNSGV1UUhzZzBKVzcyTHZVWUNCdGZ4eHhCT29GSDJOTWxCaHpMSG9DbUNoeXlDS1I0UHFXZHVqYmtKUEUzd0VyNnJjWS05bWxmRXJkYzFIcHYxRU1qYVQ1ZXNkcm95TEZlSm04NzVJRWI0TlJ1XzZpRFpfdnpNSEY3dURyN2gzQzNnN3ZaaHFNd3dIb3MzN3k2Y3MzelAwdFBCaGVndmZzQ2Mzbi04YURwQUw0Ulo2S2dDVDl0eFlyZEZOdlViLURCd3IxWDlOc3ZUS3ozYncxUl9YTzVYRl81OW9qUXoxSU1QbHJzVEtjb09ZMjN1a2F3RnFjSGdqQVBZa1RHbWZQR01FSmF0QVROMkNLcjVfZGxJTjNfRmFmLWkwNEs0RnlqWGJJWDB6WnVBVWVxS291UGFTZXc1UXZidm1WYWg4MEJRIiwiZSI6IkFRQUIiLCJkIjoiTXJGWWo5UFZ3REF1cng4LWRoUE0xMWhUSENIN1FyUWlSSGVKSHpFb2UtV3MwTWxPbXpWQnFQbnNkSjE0VU1ERHRubGdpbTlsMVFMSlNVOG0zZW0xdXZEOVdpMzE0V0M0XzNnNzRQTF9DVDBQdVZUcFI1NWhZRm5DcDVhdWRQNTNVa0lVZXpseVE0ZVRZSjdWNmhyN0R2bEUyZDY4Wk9QaWx4dVphSFNKNW8xTjB6eU1fOF9zdEdQZFRzaGNWUTlJUnV5SGhXWTZjX1hJZ0RUczBYcW0wY1dieG5EdjNWZVlGQkRxNnJXRW0zSk1uWDY0UGtpYzUtZC1hel9DNWc5RzZZaVQwU3NFeXFnV3ZPa3FXb3ZTdjlOZDhxeURGaWR2QVFkWE52R2dOMkFETkRKZ1kwVzY2a3NMZjR6Y0I3VC1jai1rbUtleGZ0LUZnLVQ4WnB1d0FRIiwicCI6IjYtWFFDRWJXNURwVzkxWFBzREZyck5ON1ZDMWZLaFZvTjlCQ2ZOOTNmTExqbG92Z3lLOE0tT3N3VTRRd0ZJdTZSLXVHdlByMHk2Q0NnTDgxeHFqY3haQ2pMa003bTNESFo3UjV2Z1RUZDNkWUFnbmNjMmlBZ2Uyamh3eG5tUWlMYTVYVU5mMEY0bXdIeFl0a2hZWTdPZk1EMmo5R1ZVZUFFeml4Ym1GSDFnVSIsInEiOiI3T29LMUx0S0NNZmlrczJpN29xRXhJX21OUjBEWmZzOG9UWmNqX1pwWlJrSWowOEJ4cEhYTGk5a2J5VUg2dnZtWXhacFRuSDgzaF9SQXc1SGpyYVAzb0J5Q3QtMy1falczeUV6aTh0OTJKaGtRVkJLT3BnNldaQlNJSWNWM05sNjRzUm96OEVyamJVelM3bWdFVjREaGd3TUJWUDlCTVg2WmoxT0drR3FSZ0UiLCJkcCI6InhvS1g4NzhaS3VubE1USW5HaEFjbWsxRkpXc2hBQnNQbnBoRXV5eWFNbmVmaVpxZ1NJRDJtNm5lLXdqc0pQNElmbWsyODJVRUJ5OUZZdTZGWkczSml2X1NNaVlselFLMDZ4STJ1Szc2X1RlUy1mUXViWGZ0WEdrTUNhTm9zcUU4SWdidGs1a2ZFSkQwWVVxU0JzTVVxQWxXbnB4TXBZc0x2aUVoUHNfaVViayIsImRxIjoiZnBJblNUSHFRcml2ZHFqQUpGc0N4WlR2YzI3VnN2VV9sZzFaOUZ1OFFSUFh1LUNFM1Zack5MU0RIdElVNGRqRVpDbkVCdkhsRzdLNTByMGRROFNMSmw5UERqb3ByRWRzWEhiN2RfTTJmN0lpMWJZVWdpdHotUWVlcU53aXRRUEhvRUU0a2Mzcy05OVQwV0FSZ1ZYTjRoNnJpV2t1b3c4MlVNcnQ0QjgyM2dFIiwicWkiOiJMck9IeTl2akt0SXdvc0RNcmVZck9yR0tEOG5FS2I4QjZPM21DQ2o4MldCRklBUUoyZm05bExQeElnNjlLalR0eEdEcnRSVXoybldKS003aE43OWhsQUZpdnlUZG1vYURHVU1nTDdaTXBSNEpTM2FJY25QYmNrQVlqcFExSWxTLTJaYTVxdEVGSV9senFsT3dITjVHajZ4UVU4WHM5OVZQeHlGYTBUUWR5ZGMifQ";
	const key = load(privateKey);
	const token = await sign(key, testClaims);
	const verifiedClaims = await verify(toPublicKey(key), token, testClaims.root);
	assert.strictEqual(verifiedClaims.root, testClaims.root);
});

test("RSA algorithms - sign and verify", async () => {
	for (const alg of ["RS256", "RS384", "RS512"] as const) {
		const { privateEncoded } = await generateAsymmetricKeyPair(alg);
		const key = load(privateEncoded);
		const token = await sign(key, testClaims);
		const verifiedClaims = await verify(toPublicKey(key), token, testClaims.root);
		assert.strictEqual(verifiedClaims.root, testClaims.root);
	}
});

test("RSA public keys verify but cannot sign", async () => {
	const { privateEncoded, publicEncoded } = await generateAsymmetricKeyPair("RS256");
	const privateKey = load(privateEncoded);
	const publicKey = loadPublic(publicEncoded);

	const token = await sign(privateKey, testClaims);
	const claims = await verify(publicKey, token, testClaims.root);
	assert.strictEqual(claims.root, testClaims.root);

	await assert.rejects(async () => {
		await sign(publicKey as Key, testClaims);
	});
});

test("PS256 algorithm - static sign and verify", async () => {
	// Key generated via rs/moq-token
	const privateKey =
		"eyJhbGciOiJQUzI1NiIsImtleV9vcHMiOlsidmVyaWZ5Iiwic2lnbiJdLCJrdHkiOiJSU0EiLCJuIjoiNDdiRHNqQmdiMVUyRlF1OG9keDZQY3I5aDFXSmlIV1FOQ2xhNzR1THFDR185VFQ0Y0xNRlJ5V1N0bDZXT2NzYjV3NnB2X3RNb3JkcG5fS3pUeDlZTGxXa2RKMlFDazZhZWxEcWtjMHR4azNwb3VVcjMyMTRMSWppaXctaUtXQ1M0bC1kMFFZOHJaa3IxYXVLUHNoUXJGdUV0WVFPY3hCajF2T2E4S0Q5YXFGTmV3cWs1WUh2Y0xIU0sxRC16SUFpTG5nbWo2OW5IYWoyNHZxS09KaURzUG4xR3FfaVBxQ2swMklOUVAwaFNvRzVUWUN1TGk3bkxrMm91TjVsQV9lRW9JNjBBOE5hWFdrZFVkWDl1UnY1c3RzMlRBX3RobGl5bWhfLXd2a2haWnl4TzVVVU5TSDZnZG5LRUlYRWh1MGRYVDRjWERfN1lrclUyTUw5cm91cnpRIiwiZSI6IkFRQUIiLCJkIjoiVTVhT1JZV2VrSi10NTVIVVgzSW9hVEJ2V2xOYTFmMlp2cHdEcG5VS0FlREpyd0FQeG9iZ2hCcFZ2WjRBOVJ3S2xRbDc1RjRoNW9UX1A5aC1XNmY4M1oxUWJnSThrcHdCOXE1blBMZ2RlbksxTmJkOElGcjF4eHRFVlptYWhDZlFJMHJJQ3FlSWRJMEtXemZKMm52N3FSazdJTXBsNTNUM3dUclJBRTJJV0xCRjcwSnVYZW1lcnVRcFBiMDhRVVVZRkM0dm5kWFpLUms0QUJYd2RPT2VlRXNPUThjVW9Fbk91NjRTMFliR2hoaVJNdHVJUjMzNG8xNDRucXR0b0E4RTBYTHFHRDRaTGxjUEZrNC1GUlh4Y3VsNlpOQmxlNWl3NEstd2hHcG5BUVQxdkc3Q21yRUd2Q09hbmZiczBNWGliUDNSZFBjZ2F2U0dmNDlRal9mX1hRIiwicCI6Ii05cEhmVGtuaXlmTHlDejNCYUIzcklCeE5WTjRBaTdtMmxUNXduVHRYWVg2VHhscS1lM0NtNXlnWGozZ3E0SGdfaG9ILTVjQVVCYTV5ZXJsOWFPbkpjQ09Ua0VmcmtBLTdmOFMxeUtKc0xrWU5Cc2hQdlpkcGpzNkZaUERNemhwTUpscEpDUlZXdVo3MHBuSDIwWTBfMXhXTzh2Q2hlallYVC1ZQURFaHFncyIsInEiOiI1M2E1bE9vRkpkaWs2WXlNSFpwT1dfcFlLTnVJelF4eGxrbjc0U2dSdlN1V3hZYkxMOXFqQjN3YnpPQkxfcUlseWNFRC1JRVdlYlVsYnhfUzAwTUZvSFB6b2ZlM2NJUjRDR0h1OVF5S3FQNlZSTUsyQmlGcUs5MS1Ja09ENnF2S3BSREFjQ0ZHcjdTZ1NWSndNd3NmSjl2WmoweGp2QXdZeHRWTjcydEJBSWMiLCJkcCI6Ikx6NmxSb0pnUHFSNmY5U2Zpaml0LW5nbHhJRWg5QmJrUzNUQlhZOGRyX3VnRnhLSGxOYmJPT0hLMjZMejhIaHV3bndUbjBpV1VHX1M3bVBZTzVvMWtzbHFhSmVpMzhkQmh0ZmdxdWJadVlNZlhUYnhwNlFEc1ZsTzdobEg5dVhRSmNQQmkzd2RYdTM1c0dvVXFiZWozWHR1MmN6QmN1bFpIVFQteUpwdTNEMCIsImRxIjoiZno5UTVTSUdkSGoycUlLZzRRRmN4TW9MUDJMNWdTaXZKVjFGQU5JamRta0pPVXhTVmR1UHR1U3U3LUg3UldCa185YUIxVk02Uk95bVNNSXBDQVdYaVU5VmlCeUVGM0pyX3NmQU02MlNhVGVVWGpuaEVkdTYzNlNqM0RoYnhGNXZTSEctS2FiUmtuVHRqWUdwdHhZTktiOS1pbjRIY25FQUNnZG9FaEJYcXU4IiwicWkiOiJuWmc0YzMwRVJKejZGS3ZOa2FXNkNmaFYyNVRmdVZkNlNOaksyUlZfZUpXczItZ2FPdDA3M2NsMEoybUN0d1RGY19BZ2g0S3VvSkJqV2hvbjFGREdnVmp6M3lyTWgxWDR5MnEyMi1PVnpOaE1ienVEdk9pNDU4Vk5uX3dJbUhCc0RBVU9EclBlR085YTJ3MVRaUjZyWXJzTml0LWJfajFWODJOSlgyajdVZW8ifQ";
	const key = load(privateKey);
	const token = await sign(key, testClaims);
	const verifiedClaims = await verify(toPublicKey(key), token, testClaims.root);
	assert.strictEqual(verifiedClaims.root, testClaims.root);
});

test("RSA-PSS algorithms - sign and verify", async () => {
	for (const alg of ["PS256", "PS384", "PS512"] as const) {
		const { privateEncoded } = await generateAsymmetricKeyPair(alg);
		const key = load(privateEncoded);
		const token = await sign(key, testClaims);
		const verifiedClaims = await verify(toPublicKey(key), token, testClaims.root);
		assert.strictEqual(verifiedClaims.root, testClaims.root);
	}
});

test("EC algorithms - sign and verify", async () => {
	for (const alg of ["ES256", "ES384"] as const) {
		const { privateEncoded } = await generateAsymmetricKeyPair(alg);
		const key = load(privateEncoded);
		const token = await sign(key, testClaims);
		const verifiedClaims = await verify(toPublicKey(key), token, testClaims.root);
		assert.strictEqual(verifiedClaims.root, testClaims.root);
	}
});

test("EdDSA algorithm - sign and verify", async () => {
	const { privateEncoded, publicEncoded } = await generateAsymmetricKeyPair("EdDSA");
	const privateKey = load(privateEncoded);
	const publicKey = loadPublic(publicEncoded);
	const token = await sign(privateKey, testClaims);
	const verifiedClaims = await verify(publicKey, token, testClaims.root);
	assert.strictEqual(verifiedClaims.root, testClaims.root);
});

test("EdDSA algorithm - static sign and verify", async () => {
	// Key generated via rs/moq-token
	const privateKey =
		"eyJhbGciOiJFZERTQSIsImtleV9vcHMiOlsidmVyaWZ5Iiwic2lnbiJdLCJrdHkiOiJPS1AiLCJjcnYiOiJFZDI1NTE5IiwieCI6Imd4cXVxMDlJUE4xVHl1TG1nTnNqZmo2NWtoa05OWndKVmp1MEEtUmQ0dkEiLCJkIjoiU1NFSHBIeTFUNHJaemhua3dpVVFlUGV1TUh2MWpLUGlxRzRsbFhyQV91cyJ9";
	const key = load(privateKey);
	const token = await sign(key, testClaims);
	const verifiedClaims = await verify(toPublicKey(key), token, testClaims.root);
	assert.strictEqual(verifiedClaims.root, testClaims.root);
});

test("EdDSA algorithm - verify with private key fails", async () => {
	// Key generated via rs/moq-token
	const privateKey =
		"eyJhbGciOiJFZERTQSIsImtleV9vcHMiOlsidmVyaWZ5Iiwic2lnbiJdLCJrdHkiOiJPS1AiLCJjcnYiOiJFZDI1NTE5IiwieCI6Imd4cXVxMDlJUE4xVHl1TG1nTnNqZmo2NWtoa05OWndKVmp1MEEtUmQ0dkEiLCJkIjoiU1NFSHBIeTFUNHJaemhua3dpVVFlUGV1TUh2MWpLUGlxRzRsbFhyQV91cyJ9";
	const key = load(privateKey);
	const token = await sign(key, testClaims);
	await assert.rejects(async () => {
		await verify(key, token, testClaims.root);
	});
});

test("asymmetric cross-algorithm verification fails", async () => {
	const { privateEncoded: rsPrivate, publicEncoded: rsPublic } = await generateAsymmetricKeyPair("RS256");
	const { publicEncoded: psPublic } = await generateAsymmetricKeyPair("PS256");
	const rsKey = load(rsPrivate);
	const rsPublicKey = loadPublic(rsPublic);
	const psPublicKey = loadPublic(psPublic);
	const token = await sign(rsKey, testClaims);

	const verifiedClaims = await verify(rsPublicKey, token, testClaims.root);
	assert.strictEqual(verifiedClaims.root, testClaims.root);

	await assert.rejects(async () => {
		await verify(psPublicKey, token, testClaims.root);
	});
});

test("cross-algorithm verification fails", async () => {
	const hs256Key = load(encodeJwk(testKey));
	const hs384Key = load(
		encodeJwk({
			alg: "HS384",
			key_ops: ["sign", "verify"],
			kty: "oct",
			k: "dGVzdC1zZWNyZXQtdGhhdC1pcy1sb25nLWVub3VnaC1mb3ItaG1hYy1zaGEzODQtYWxnb3JpdGhtLXRlc3RpbmctcHVycG9zZXM",
			kid: "test-key-hs384",
		}),
	);

	const token = await sign(hs256Key, testClaims);

	await assert.rejects(async () => {
		await verify(hs384Key, token, testClaims.root);
	});
});

test("load - invalid algorithm", () => {
	const invalidKey = {
		...testKey,
		alg: "ES512", // unsupported algorithm
	};
	const jwk = encodeJwk(invalidKey);

	assert.throws(() => {
		load(jwk);
	});
});

test("load - mismatched algorithm", () => {
	const invalidKey = {
		...testKey,
		alg: "RS256", // mismatched algorithm for oct key
	};
	const jwk = encodeJwk(invalidKey);

	assert.throws(() => {
		load(jwk);
	});
});

test("load - invalid key_ops", () => {
	const invalidKey = {
		...testKey,
		key_ops: ["invalid-operation"],
	};
	const jwk = encodeJwk(invalidKey);

	assert.throws(() => {
		load(jwk);
	});
});

test("load - missing alg field", () => {
	const invalidKey = {
		key_ops: ["sign", "verify"],
		kty: "oct",
		k: testKey.k,
	};
	const jwk = encodeJwk(invalidKey);

	assert.throws(() => {
		load(jwk);
	});
});

test("sign - includes kid in header when present", async () => {
	const key = load(encodeJwk(testKey));
	const token = await sign(key, testClaims);

	// Decode the header to verify kid is present
	const [headerB64] = token.split(".");
	const headerBuffer = base64.toArrayBuffer(headerB64, true); // true for urlSafe
	const header = JSON.parse(new TextDecoder().decode(headerBuffer));

	assert.strictEqual(header.kid, "test-key-1");
	assert.strictEqual(header.alg, "HS256");
	assert.strictEqual(header.typ, "JWT");
});

test("sign - no kid in header when not present", async () => {
	const keyWithoutKid = {
		...testKey,
		kid: undefined,
	};
	delete keyWithoutKid.kid;

	const key = load(encodeJwk(keyWithoutKid));
	const token = await sign(key, testClaims);

	// Decode the header to verify kid is not present
	const [headerB64] = token.split(".");
	const headerBuffer = base64.toArrayBuffer(headerB64, true); // true for urlSafe
	const header = JSON.parse(new TextDecoder().decode(headerBuffer));

	assert.strictEqual(header.kid, undefined);
	assert.strictEqual(header.alg, "HS256");
	assert.strictEqual(header.typ, "JWT");
});

test("sign - sets issued at timestamp", async () => {
	const key = load(encodeJwk(testKey));
	const claimsWithoutIat: Claims = {
		root: "test-path",
		put: "test-pub",
	};

	const beforeSign = Math.floor(Date.now() / 1000);
	const token = await sign(key, claimsWithoutIat);
	const afterSign = Math.floor(Date.now() / 1000);

	// Decode the payload to verify iat is set
	const [, payloadB64] = token.split(".");
	const payloadBuffer = base64.toArrayBuffer(payloadB64, true); // true for urlSafe
	const payload = JSON.parse(new TextDecoder().decode(payloadBuffer));

	assert.ok(payload.iat >= beforeSign);
	assert.ok(payload.iat <= afterSign);
});

test("verify - malformed token parts", async () => {
	const key = load(encodeJwk(testKey));

	await assert.rejects(async () => {
		await verify(key, "invalid", "test-path");
	});

	await assert.rejects(async () => {
		await verify(key, "invalid.token", "test-path");
	});

	await assert.rejects(async () => {
		await verify(key, "invalid.token.signature.extra", "test-path");
	});
});

test("verify - invalid payload structure", async () => {
	const key = load(encodeJwk(testKey));

	// Create a token with invalid payload structure
	const headerData = new TextEncoder().encode(JSON.stringify({ alg: "HS256", typ: "JWT" }));
	const header = base64.fromArrayBuffer(headerData.buffer as ArrayBuffer, true); // true for urlSafe

	const payloadData = new TextEncoder().encode(JSON.stringify({ invalid: "payload" }));
	const payload = base64.fromArrayBuffer(payloadData.buffer as ArrayBuffer, true); // true for urlSafe
	const signature = "invalid";
	const invalidToken = `${header}.${payload}.${signature}`;

	await assert.rejects(async () => {
		await verify(key, invalidToken, "test-path");
	});
});

test("verify - claims validation during verification", async () => {
	const key = load(encodeJwk(testKey));

	// We need to create a token with valid claims since sign() would reject invalid ones
	const token = await sign(key, { root: "test-path", put: "/absolute-pub" });

	// Test that valid tokens pass verification
	const verifiedClaims = await verify(key, token, "test-path");
	assert.strictEqual(verifiedClaims.root, "test-path");
});
