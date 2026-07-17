import { expect, test } from "bun:test";
import { generate } from "./generate.ts";
import { sign } from "./key.ts";
import { findKey, type KeySet, loadSet, signWith, toPublicSet, verifyWith } from "./set.ts";

const CLAIMS = { root: "demo", put: ["alice"], get: [""] };

async function keySet(...kids: (string | undefined)[]): Promise<KeySet> {
	return { keys: await Promise.all(kids.map((kid) => generate("HS256", kid))) };
}

test("loadSet - parses a JWKS", async () => {
	const set = await keySet("key-1", "key-2");
	const loaded = loadSet(JSON.stringify(set));

	expect(loaded.keys.length).toBe(2);
	expect(findKey(loaded, "key-1")?.kid).toBe("key-1");
	expect(findKey(loaded, "nope")).toBeUndefined();
});

test("loadSet - rejects invalid JSON and invalid keys", () => {
	expect(() => loadSet("not json")).toThrow(/invalid JSON/);
	expect(() => loadSet(`{"keys":[{"kty":"oct","alg":"HS256"}]}`)).toThrow(/Failed to validate JWKS/);
});

test("signWith / verifyWith - round trip selects the key by kid", async () => {
	const set = await keySet("key-1", "key-2");

	const token = await signWith(set, CLAIMS);
	const claims = await verifyWith(set, token);
	expect(claims.root).toBe("demo");
	expect(claims.put).toEqual(["alice"]);
});

test("verifyWith - a token signed by a key outside the set is rejected", async () => {
	const set = await keySet("key-1");
	const stranger = await generate("HS256", "key-2");

	const token = await sign(stranger, CLAIMS);
	await expect(verifyWith(set, token)).rejects.toThrow(/Cannot find key with kid key-2/);
});

test("verifyWith - a kid-less token is accepted only when the set holds one key", async () => {
	const key = await generate("HS256");
	key.kid = undefined;
	const token = await sign(key, CLAIMS);

	expect((await verifyWith({ keys: [key] }, token)).root).toBe("demo");

	const other = await generate("HS256");
	other.kid = undefined;
	await expect(verifyWith({ keys: [key, other] }, token)).rejects.toThrow(/Missing kid/);
});

test("signWith - fails when no key can sign", async () => {
	const set = await keySet("key-1");
	set.keys[0].key_ops = ["verify"];

	await expect(signWith(set, CLAIMS)).rejects.toThrow(/Cannot find signing key/);
});

test("toPublicSet - strips private material", async () => {
	const key = await generate("ES256", "key-1");
	const publicSet = toPublicSet({ keys: [key] });

	expect(publicSet.keys.length).toBe(1);
	expect(publicSet.keys[0].kid).toBe("key-1");
	expect("d" in publicSet.keys[0]).toBe(false);
	expect(publicSet.keys[0].key_ops).not.toContain("sign");
});

test("toPublicSet - rejects a symmetric key, which has no public half", async () => {
	const set = await keySet("key-1");
	expect(() => toPublicSet(set)).toThrow(/Cannot derive public key/);
});
