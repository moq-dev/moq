import { expect, test } from "bun:test";
import { proveRouteKey, sign } from "./route_key.ts";

// PKCS#8 v1 (RFC 8410) prefix of an Ed25519 key, up to its 32-byte seed.
const PKCS8_SEED_PREFIX = [
	0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

// Shared with rs/moq-net/src/route/proof.rs, so both sides sign the same transcript.
const SEED = new Uint8Array(32).fill(1);
const SECRET = new Uint8Array(32).fill(2);
const CLIENT_PROOF =
	"8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c" +
	"e7ea4e34a65b8501185b805d09b58a61b5b8495654fcaac0949d4f98aa35002e" +
	"6fca096329973e91e1105030dd88fa0eb543c68c00e18394f8300cb0574cf901";

async function keyFromSeed(seed: Uint8Array): Promise<CryptoKeyPair> {
	const der = new Uint8Array([...PKCS8_SEED_PREFIX, ...seed]);
	const privateKey = await crypto.subtle.importKey("pkcs8", der, { name: "Ed25519" }, true, ["sign"]);
	// WebCrypto derives the public key only through a JWK export.
	const jwk = await crypto.subtle.exportKey("jwk", privateKey);
	const publicKey = await crypto.subtle.importKey(
		"jwk",
		{ kty: jwk.kty, crv: jwk.crv, x: jwk.x },
		{ name: "Ed25519" },
		true,
		["verify"],
	);
	return { privateKey, publicKey };
}

function hex(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

test("a proof matches the Rust transcript", async () => {
	const proof = await sign(await keyFromSeed(SEED), SECRET, true);
	expect(hex(proof)).toBe(CLIENT_PROOF);
});

test("the prover's side changes the signature", async () => {
	const key = await keyFromSeed(SEED);
	const client = await sign(key, SECRET, true);
	const server = await sign(key, SECRET, false);
	expect(client.subarray(0, 32)).toEqual(server.subarray(0, 32));
	expect(client.subarray(32)).not.toEqual(server.subarray(32));
});

test("a transport without an exporter proves nothing", async () => {
	expect(await proveRouteKey({} as WebTransport, true)).toBeUndefined();
});

test("a transport with an exporter proves one key per page", async () => {
	const labels: string[] = [];
	const transport = {
		exportKeyingMaterial: async (label: BufferSource, context: BufferSource, length: number) => {
			labels.push(new TextDecoder().decode(label));
			expect((context as Uint8Array).byteLength).toBe(0);
			return new Uint8Array(length).fill(3);
		},
	} as unknown as WebTransport;
	const first = await proveRouteKey(transport, true);
	const second = await proveRouteKey(transport, false);
	expect(first?.byteLength).toBe(96);
	expect(first?.subarray(0, 32)).toEqual(second?.subarray(0, 32));
	expect(labels).toEqual(["EXPORTER-moq-lite-route-key", "EXPORTER-moq-lite-route-key"]);
});

test("Firefox's exporter proves nothing", async () => {
	// neqo derives a secret the draft's peers never match, so a proof would end the session.
	const exporter = { exportKeyingMaterial: async () => new Uint8Array(32) } as unknown as WebTransport;
	const userAgent = "Mozilla/5.0 (X11; Linux x86_64; rv:157.0) Gecko/20100101 Firefox/157.0";
	const own = Object.getOwnPropertyDescriptor(navigator, "userAgent");
	Object.defineProperty(navigator, "userAgent", { value: userAgent, configurable: true });
	try {
		expect(await proveRouteKey(exporter, true)).toBeUndefined();
	} finally {
		if (own) Object.defineProperty(navigator, "userAgent", own);
		else Reflect.deleteProperty(navigator, "userAgent");
	}
});

test("unavailable Ed25519 leaves this page unproven across sessions", () => {
	// A child has its own page capability cache, independent of the successful proof tests.
	const moduleUrl = new URL("./route_key.ts", import.meta.url).href;
	const child = Bun.spawnSync([
		process.execPath,
		"-e",
		`
		import { strict as assert } from "node:assert";
		import { proveRouteKey } from ${JSON.stringify(moduleUrl)};
		let calls = 0;
		crypto.subtle.generateKey = async () => { calls++; throw new Error("unavailable"); };
		const transport = { exportKeyingMaterial: async () => new Uint8Array(32) };
		assert.equal(await proveRouteKey(transport, true), undefined);
		assert.equal(await proveRouteKey(transport, true), undefined);
		assert.equal(calls, 1);
		`,
	]);
	expect(child.stderr.toString()).toBe("");
	expect(child.exitCode).toBe(0);
});
