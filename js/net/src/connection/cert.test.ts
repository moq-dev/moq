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

test("hex round-trips through fromBytes/toBytes", () => {
	expect(Hex.fromBytes(Hex.toBytes(ABC_SHA256))).toBe(ABC_SHA256);
});
