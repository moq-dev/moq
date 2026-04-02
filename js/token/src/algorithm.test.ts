import { expect, test } from "bun:test";
import { AlgorithmSchema } from "./algorithm.ts";

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
		expect(AlgorithmSchema.parse(alg)).toBe(alg);
	}
});

test("algorithm schema - invalid algorithms", () => {
	expect(() => {
		AlgorithmSchema.parse("HS128");
	}).toThrow(/Invalid option/);

	expect(() => {
		AlgorithmSchema.parse("ES512");
	}).toThrow(/Invalid option/);

	expect(() => {
		AlgorithmSchema.parse("invalid");
	}).toThrow(/Invalid option/);

	expect(() => {
		AlgorithmSchema.parse("");
	}).toThrow(/Invalid option/);
});

test("algorithm schema - type safety", () => {
	// Test that TypeScript types are working correctly
	const validAlgorithm = AlgorithmSchema.parse("HS256");
	expect(typeof validAlgorithm === "string").toBeTruthy();
	expect(
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
	).toBeTruthy();
});

test("algorithm schema - case sensitivity", () => {
	expect(() => {
		AlgorithmSchema.parse("hs256");
	}).toThrow(/Invalid option/);

	expect(() => {
		AlgorithmSchema.parse("Hs256");
	}).toThrow(/Invalid option/);

	expect(() => {
		AlgorithmSchema.parse("HS256 ");
	}).toThrow(/Invalid option/);
});

test("algorithm schema - non-string inputs", () => {
	expect(() => {
		AlgorithmSchema.parse(256);
	}).toThrow(/Expected string|Invalid option/);

	expect(() => {
		AlgorithmSchema.parse(null);
	}).toThrow(/Expected string|Invalid option/);

	expect(() => {
		AlgorithmSchema.parse(undefined);
	}).toThrow(/Expected string|Invalid option/);

	expect(() => {
		AlgorithmSchema.parse({});
	}).toThrow(/Expected string|Invalid option/);
});
