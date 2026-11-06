import { expect, test } from "bun:test";
import * as Hex from "../util/hex.ts";
import { certificateHash } from "./connect.ts";

// SHA-256("abc"), a well-known test vector. We feed "abc" as the cert bytes so
// the expected digest is independent of our own implementation.
const ABC = new TextEncoder().encode("abc");
const ABC_SHA256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

test("certificateHash hashes raw DER bytes", async () => {
	const hash = await certificateHash(ABC);
	expect(Hex.fromBytes(hash)).toBe(ABC_SHA256);
});

test("certificateHash decodes PEM armor before hashing", async () => {
	// base64("abc") === "YWJj"
	const pem = "-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n";
	const hash = await certificateHash(pem);
	expect(Hex.fromBytes(hash)).toBe(ABC_SHA256);
});

test("certificateHash rejects a PEM string without armor", async () => {
	// Valid base64, but no -----BEGIN CERTIFICATE----- wrapper.
	await expect(certificateHash("YWJj")).rejects.toThrow(/armor/);
});

test("certificateHash rejects PEM armor without an END line or a body", async () => {
	await expect(certificateHash("-----BEGIN CERTIFICATE-----\nYWJj\n")).rejects.toThrow(/armor/);
	await expect(certificateHash("-----BEGIN CERTIFICATE-----\n\n-----END CERTIFICATE-----")).rejects.toThrow(/armor/);
});

test("certificateHash rejects repeated BEGIN lines in linear time", async () => {
	// A lazy regex rescans to the end from every BEGIN, which takes tens of seconds here
	// and trips the test timeout; a linear scan finishes in well under a millisecond.
	const pem = "-----BEGIN CERTIFICATE-----a".repeat(40_000);
	await expect(certificateHash(pem)).rejects.toThrow(/armor/);
});
