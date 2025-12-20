import assert from "node:assert";
import test from "node:test";
import { AlgorithmSchema } from "./algorithm";

test("algorithm schema - valid algorithms", () => {
	const validAlgorithms = [
		"HS256",
		"HS384",
		"HS512",
		"ES256",
		"ES384",
		"RS256",
		"RS384",
		"RS512",
		"PS256",
		"PS384",
		"PS512",
		"EdDSA",
	] as const;

	for (const alg of validAlgorithms) {
		assert.strictEqual(AlgorithmSchema.parse(alg), alg);
	}
});

test("algorithm schema - invalid algorithms", () => {
	assert.throws(() => {
		AlgorithmSchema.parse("HS128");
	}, /Invalid option/);

	assert.throws(() => {
		AlgorithmSchema.parse("ES512");
	}, /Invalid option/);

	assert.throws(() => {
		AlgorithmSchema.parse("invalid");
	}, /Invalid option/);

	assert.throws(() => {
		AlgorithmSchema.parse("");
	}, /Invalid option/);
});

test("algorithm schema - type safety", () => {
	// Test that TypeScript types are working correctly
	const validAlgorithm = AlgorithmSchema.parse("HS256");
	assert.ok(typeof validAlgorithm === "string");
	assert.ok(
		[
			"HS256",
			"HS384",
			"HS512",
			"ES256",
			"ES384",
			"RS256",
			"RS384",
			"RS512",
			"PS256",
			"PS384",
			"PS512",
			"EdDSA",
		].includes(validAlgorithm),
	);
});

test("algorithm schema - case sensitivity", () => {
	assert.throws(() => {
		AlgorithmSchema.parse("hs256");
	}, /Invalid option/);

	assert.throws(() => {
		AlgorithmSchema.parse("Hs256");
	}, /Invalid option/);

	assert.throws(() => {
		AlgorithmSchema.parse("HS256 ");
	}, /Invalid option/);
});

test("algorithm schema - non-string inputs", () => {
	assert.throws(() => {
		AlgorithmSchema.parse(256);
	}, /Expected string|Invalid option/);

	assert.throws(() => {
		AlgorithmSchema.parse(null);
	}, /Expected string|Invalid option/);

	assert.throws(() => {
		AlgorithmSchema.parse(undefined);
	}, /Expected string|Invalid option/);

	assert.throws(() => {
		AlgorithmSchema.parse({});
	}, /Expected string|Invalid option/);
});
