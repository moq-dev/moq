// Replays rs/moq-auth/tests/corpus.json, which the Rust crate replays too, so the two
// parsers cannot disagree on a token, key, key set, or grant without both failing.
import { describe, expect, test } from "bun:test";
import * as jose from "jose";
import { type Access, type Claims, patterns } from "./claims.ts";
import { GrantSchema } from "./contract.ts";
import { Key } from "./key.ts";
import { KeySet } from "./set.ts";

type Case = { name: string; jwk?: unknown; jwks?: unknown; grant?: unknown; valid: boolean };

const corpus = (await Bun.file(new URL("../../../rs/moq-auth/tests/corpus.json", import.meta.url)).json()) as {
	keys: Record<string, unknown>;
	tokens: { name: string; key: string; token?: string; payload?: jose.JWTPayload; claims: unknown }[];
	jwk: Case[];
	sets: Case[];
	grants: Case[];
};

// The claims as the corpus writes them: patterns reduced, absent roles empty, a capped role
// as its wire object, and times and the token ID only when present.
function canonical(claims: Claims): unknown {
	const role = (access: Access | undefined) =>
		access === undefined || Array.isArray(access)
			? patterns(access).toJSON()
			: { path: patterns(access.path).toJSON(), rate: access.rate };
	return {
		root: claims.root,
		publish: role(claims.publish),
		subscribe: role(claims.subscribe),
		...(claims.expires !== undefined && { exp: claims.expires }),
		...(claims.issued !== undefined && { iat: claims.issued }),
		...(claims.notBefore !== undefined && { nbf: claims.notBefore }),
		...(claims.id !== undefined && { jti: claims.id }),
	};
}

describe("tokens", () => {
	for (const c of corpus.tokens) {
		test(c.name, async () => {
			const key = Key.parse(JSON.stringify(corpus.keys[c.key]));
			if ((c.token === undefined) === (c.payload === undefined))
				throw new Error("give exactly one of token or payload");
			// Sign the raw payload, bypassing `Key.sign`'s own checks, so verification sees it as written.
			const token =
				c.token ??
				(await new jose.SignJWT(c.payload)
					.setProtectedHeader({ alg: key.alg, ...(key.kid && { kid: key.kid }) })
					.sign(await jose.importJWK(key as jose.JWK, key.alg)));
			const verified = Key.verify(key as Parameters<typeof Key.verify>[0], token);
			if (c.claims === null) {
				await expect(verified).rejects.toThrow();
			} else {
				expect(canonical(await verified)).toEqual(c.claims);
			}
		});
	}
});

// A key or set given as a string is its raw text, for what a JSON value cannot spell,
// such as a repeated member.
const text = (document: unknown) => (typeof document === "string" ? document : JSON.stringify(document));

const sections: [string, Case[], (input: unknown) => unknown][] = [
	["jwk", corpus.jwk, (jwk) => Key.parse(text(jwk))],
	["sets", corpus.sets, (jwks) => KeySet.parse(text(jwks))],
	["grants", corpus.grants, (grant) => GrantSchema.parse(grant)],
];
for (const [section, cases, parse] of sections) {
	describe(section, () => {
		for (const c of cases) {
			test(c.name, () => {
				const run = () => parse(c.jwk ?? c.jwks ?? c.grant);
				if (c.valid) run();
				else expect(run).toThrow();
			});
		}
	});
}
