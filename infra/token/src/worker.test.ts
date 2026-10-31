import { describe, expect, test } from "bun:test";

import worker, { AUDIENCE, authorize, type Claims, Denied, ISSUER, POLICIES, verify } from "./worker";

const land = POLICIES["/land"];
const now = 1_800_000_000;

const valid: Claims = {
	iss: ISSUER,
	aud: AUDIENCE,
	exp: now + 300,
	nbf: now - 10,
	repository: "moq-dev/quest",
	repository_id: "42",
	repository_owner: "moq-dev",
	repository_owner_id: "248218914",
	ref: "refs/heads/main",
	workflow_ref: "moq-dev/quest/.github/workflows/land.yml@refs/heads/main",
	job_workflow_ref: "moq-dev/quest/.github/workflows/land.yml@refs/heads/main",
};

describe("authorize", () => {
	test("main's land workflow in a moq-dev repository gets that repository", () => {
		expect(authorize(valid, land, now)).toEqual({ repository: "moq-dev/quest", id: 42 });
	});

	test("a direct job without job_workflow_ref gets it too", () => {
		expect(authorize({ ...valid, job_workflow_ref: undefined }, land, now)).toEqual({
			repository: "moq-dev/quest",
			id: 42,
		});
	});

	test.each<[string, Partial<Claims>, string]>([
		["another issuer", { iss: "https://example.com" }, "issuer"],
		["another audience", { aud: "sts.amazonaws.com" }, "audience"],
		["an expired token", { exp: now }, "expired"],
		["a future token", { nbf: now + 120 }, "not yet valid"],
		["another owner", { repository_owner: "kixelated" }, "owner"],
		["a recreated owner name", { repository_owner_id: "1" }, "owner"],
		["a repository outside the owner", { repository: "kixelated/quest" }, "repository"],
		["a pushed branch", { ref: "refs/heads/land" }, "ref"],
		["a pull request", { ref: "refs/pull/7/merge" }, "ref"],
		["a tag", { ref: "refs/tags/v1" }, "ref"],
		[
			"another workflow on main",
			{ workflow_ref: "moq-dev/quest/.github/workflows/check.yml@refs/heads/main" },
			"workflow",
		],
		[
			"the land workflow from a branch",
			{ workflow_ref: "moq-dev/quest/.github/workflows/land.yml@refs/heads/land" },
			"workflow",
		],
		[
			"another repository's land workflow",
			{ workflow_ref: "moq-dev/moq/.github/workflows/land.yml@refs/heads/main" },
			"workflow",
		],
		[
			"a job of a workflow the land workflow calls",
			{ job_workflow_ref: "moq-dev/quest/.github/workflows/check.yml@refs/heads/main" },
			"job workflow",
		],
	])("denies %s", (_, change, reason) => {
		expect(() => authorize({ ...valid, ...change }, land, now)).toThrow(new Denied(reason));
	});
});

const RS256 = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" };

async function keypair(kid: string) {
	const pair = (await crypto.subtle.generateKey(
		{ ...RS256, modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]) },
		true,
		["sign", "verify"],
	)) as CryptoKeyPair;
	const jwk = { ...(await crypto.subtle.exportKey("jwk", pair.publicKey)), kid };
	return { privateKey: pair.privateKey, jwk };
}

const encode = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url");

async function sign(key: CryptoKey, header: object, claims: Claims): Promise<string> {
	const body = `${encode(header)}.${encode(claims)}`;
	const signature = await crypto.subtle.sign(RS256, key, new TextEncoder().encode(body));
	return `${body}.${Buffer.from(signature).toString("base64url")}`;
}

describe("verify", async () => {
	const github = await keypair("github");
	const other = await keypair("other");
	const jwks = { keys: [github.jwk] };

	test("returns the claims GitHub's key signed", async () => {
		expect(await verify(await sign(github.privateKey, { alg: "RS256", kid: "github" }, valid), jwks)).toEqual(
			valid,
		);
	});

	test("denies another key claiming GitHub's kid", async () => {
		const token = await sign(other.privateKey, { alg: "RS256", kid: "github" }, valid);
		await expect(verify(token, jwks)).rejects.toThrow(new Denied("signature"));
	});

	test("denies an unknown kid", async () => {
		const token = await sign(other.privateKey, { alg: "RS256", kid: "other" }, valid);
		await expect(verify(token, jwks)).rejects.toThrow(new Denied("unknown key"));
	});

	test("denies an unsigned token", async () => {
		const token = `${encode({ alg: "none", kid: "github" })}.${encode(valid)}.`;
		await expect(verify(token, jwks)).rejects.toThrow(new Denied("algorithm"));
	});

	test.each([
		["no segments", "garbage"],
		["a non-JSON header", "a.b.c"],
		["a non-object header", `${encode(7)}.${encode(valid)}.sig`],
		["an undecodable signature", `${encode({ alg: "RS256", kid: "github" })}.${encode(valid)}.%%%`],
	])("denies %s as malformed", async (_, token) => {
		await expect(verify(token, jwks)).rejects.toThrow(new Denied("malformed"));
	});

	test("denies changed claims", async () => {
		const [header, , signature] = (await sign(github.privateKey, { alg: "RS256", kid: "github" }, valid)).split(
			".",
		);
		const forged = `${header}.${encode({ ...valid, repository: "moq-dev/moq" })}.${signature}`;
		await expect(verify(forged, jwks)).rejects.toThrow(new Denied("signature"));
	});
});

describe("fetch", async () => {
	const github = await keypair("github");
	const app = (await crypto.subtle.generateKey(
		{ ...RS256, modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]) },
		true,
		["sign", "verify"],
	)) as CryptoKeyPair;
	const pkcs8 = Buffer.from(await crypto.subtle.exportKey("pkcs8", app.privateKey)).toString("base64");
	const env = {
		APP_CLIENT_ID: "Iv23test",
		APP_PRIVATE_KEY: `-----BEGIN PRIVATE KEY-----\n${pkcs8.match(/.{1,64}/g)?.join("\n")}\n-----END PRIVATE KEY-----\n`,
	};

	/** Runs the worker against fake GitHub endpoints, recording each API call. */
	async function call(path: string, init: RequestInit) {
		const calls: { url: string; auth: string | null; body?: unknown }[] = [];
		const real = globalThis.fetch;
		globalThis.fetch = (async (input: string | URL | Request, options?: RequestInit) => {
			const url = String(input);
			if (url === `${ISSUER}/.well-known/jwks`) return Response.json({ keys: [github.jwk] });
			const auth = new Headers(options?.headers).get("Authorization");
			calls.push({ url, auth, body: options?.body ? JSON.parse(String(options.body)) : undefined });
			if (url.endsWith("/installation")) return Response.json({ id: 7 });
			if (url.endsWith("/access_tokens")) return Response.json({ token: "ghs_test", expires_at: "later" });
			return new Response("unexpected", { status: 500 });
		}) as typeof fetch;
		try {
			const response = await worker.fetch(new Request(`https://token.moq.dev${path}`, init), env);
			return { response, calls };
		} finally {
			globalThis.fetch = real;
		}
	}

	const live = () => ({ ...valid, exp: Math.floor(Date.now() / 1000) + 300, nbf: undefined });

	test("issues a token scoped to the caller's repository and the policy's permissions", async () => {
		const token = await sign(github.privateKey, { alg: "RS256", kid: "github" }, live());
		const { response, calls } = await call("/land", {
			method: "POST",
			headers: { Authorization: `Bearer ${token}` },
		});
		expect(response.status).toBe(200);
		expect(await response.json()).toEqual({ token: "ghs_test", expires_at: "later" });
		expect(calls.map((c) => c.url)).toEqual([
			"https://api.github.com/repos/moq-dev/quest/installation",
			"https://api.github.com/app/installations/7/access_tokens",
		]);
		expect(calls[1].body).toEqual({ repository_ids: [42], permissions: { contents: "write", workflows: "write" } });

		// The app JWT is signed by the stored key and names the client ID.
		const [header, payload, signature] = (calls[0].auth ?? "").replace("Bearer ", "").split(".");
		const signed = new TextEncoder().encode(`${header}.${payload}`);
		expect(await crypto.subtle.verify(RS256, app.publicKey, Buffer.from(signature, "base64url"), signed)).toBe(
			true,
		);
		expect(JSON.parse(Buffer.from(payload, "base64url").toString()).iss).toBe("Iv23test");
	});

	test("denies without calling GitHub when the policy refuses", async () => {
		const token = await sign(
			github.privateKey,
			{ alg: "RS256", kid: "github" },
			{ ...live(), ref: "refs/heads/evil" },
		);
		const { response, calls } = await call("/land", {
			method: "POST",
			headers: { Authorization: `Bearer ${token}` },
		});
		expect(response.status).toBe(403);
		expect(await response.text()).toBe("denied: ref\n");
		expect(calls).toEqual([]);
	});

	test("denies a request without a bearer token", async () => {
		const { response, calls } = await call("/land", { method: "POST" });
		expect(response.status).toBe(403);
		expect(calls).toEqual([]);
	});

	test("knows only its policies' paths", async () => {
		expect((await call("/admin", { method: "POST" })).response.status).toBe(404);
		expect((await call("/land", { method: "GET" })).response.status).toBe(405);
	});
});
