import { expect, test } from "bun:test";
import { decodeProtectedHeader } from "jose";
import { Key } from "./key.ts";
import { KeySet } from "./set.ts";

const CLAIMS = { root: "demo", publish: ["alice/**"], subscribe: ["**"] };

async function keySet(...kids: (string | undefined)[]): Promise<KeySet> {
	return { keys: await Promise.all(kids.map((kid) => Key.generate("HS256", kid))) };
}

test("parse - parses a JWKS", async () => {
	const set = await keySet("key-1", "key-2");
	const loaded = KeySet.parse(JSON.stringify(set));

	expect(loaded.keys.length).toBe(2);
	expect(KeySet.find(loaded, "key-1")?.kid).toBe("key-1");
	expect(KeySet.find(loaded, "nope")).toBeUndefined();
});

test("parse - rejects invalid JSON and invalid keys", () => {
	expect(() => KeySet.parse("not json")).toThrow(/invalid JSON/);
	expect(() => KeySet.parse(`{"keys":[{"kty":"oct","alg":"HS256"}]}`)).toThrow(/Failed to validate JWKS/);
});

test("sign / verify - round trip selects the key by kid", async () => {
	const set = await keySet("key-1", "key-2");

	const token = await KeySet.sign(set, CLAIMS);
	const claims = await KeySet.verify(set, token);
	expect(claims.root).toBe("demo");
	expect(claims.publish).toEqual(["alice/**"]);
});

test("verify - a token signed by a key outside the set is rejected", async () => {
	const set = await keySet("key-1");
	const stranger = await Key.generate("HS256", "key-2");

	const token = await Key.sign(stranger, CLAIMS);
	await expect(KeySet.verify(set, token)).rejects.toThrow(/Cannot find key with kid key-2/);
});

test("verify - a kid-less token is accepted only when the set holds one key", async () => {
	const key = await Key.generate("HS256");
	key.kid = undefined;
	const token = await Key.sign(key, CLAIMS);

	expect((await KeySet.verify({ keys: [key] }, token)).root).toBe("demo");

	const other = await Key.generate("HS256");
	other.kid = undefined;
	await expect(KeySet.verify({ keys: [key, other] }, token)).rejects.toThrow(/Missing kid/);
});

test("sign - fails when no key can sign", async () => {
	const set = await keySet("key-1");
	set.keys[0].key_ops = ["verify"];

	await expect(KeySet.sign(set, CLAIMS)).rejects.toThrow(/Cannot find signing key/);
});

test("sign - skips a public-only key that claims sign", async () => {
	const privateKey = await Key.generate("ES256", "active");

	const publicOnly = await Key.generate("ES256", "old");
	if (publicOnly.kty !== "EC") throw new Error("expected an EC key");
	delete publicOnly.d;
	publicOnly.key_ops = ["sign", "verify"];

	const set: KeySet = { keys: [publicOnly, privateKey] };
	const token = await KeySet.sign(set, CLAIMS);
	expect(decodeProtectedHeader(token).kid).toBe("active");
});

test("public - strips private material", async () => {
	const key = await Key.generate("ES256", "key-1");
	const publicSet = KeySet.public({ keys: [key] });

	expect(publicSet.keys.length).toBe(1);
	expect(publicSet.keys[0].kid).toBe("key-1");
	expect("d" in publicSet.keys[0]).toBe(false);
	expect(publicSet.keys[0].key_ops).not.toContain("sign");
});

test("public - rejects a symmetric key, which has no public half", async () => {
	const set = await keySet("key-1");
	expect(() => KeySet.public(set)).toThrow(/Cannot derive public key/);
});
