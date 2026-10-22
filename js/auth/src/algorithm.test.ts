import { expect, test } from "bun:test";
import { AlgorithmSchema } from "./algorithm.ts";

test("algorithm schema - invalid algorithms", () => {
	expect(() => {
		AlgorithmSchema.parse("HS128");
	}).toThrow(/Invalid option|Invalid input/);

	expect(() => {
		AlgorithmSchema.parse("ES512");
	}).toThrow(/Invalid option|Invalid input/);

	expect(() => {
		AlgorithmSchema.parse("invalid");
	}).toThrow(/Invalid option|Invalid input/);

	expect(() => {
		AlgorithmSchema.parse("");
	}).toThrow(/Invalid option|Invalid input/);
});

test("algorithm schema - case sensitivity", () => {
	expect(() => {
		AlgorithmSchema.parse("hs256");
	}).toThrow(/Invalid option|Invalid input/);

	expect(() => {
		AlgorithmSchema.parse("Hs256");
	}).toThrow(/Invalid option|Invalid input/);

	expect(() => {
		AlgorithmSchema.parse("HS256 ");
	}).toThrow(/Invalid option|Invalid input/);
});

test("algorithm schema - non-string inputs", () => {
	expect(() => {
		AlgorithmSchema.parse(256);
	}).toThrow(/Expected string|Invalid option|Invalid input/);

	expect(() => {
		AlgorithmSchema.parse(null);
	}).toThrow(/Expected string|Invalid option|Invalid input/);

	expect(() => {
		AlgorithmSchema.parse(undefined);
	}).toThrow(/Expected string|Invalid option|Invalid input/);

	expect(() => {
		AlgorithmSchema.parse({});
	}).toThrow(/Expected string|Invalid option|Invalid input/);
});
