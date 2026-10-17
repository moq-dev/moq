import { expect, test } from "bun:test";
import vectors from "../../../rs/moq-auth/rate.json" with { type: "json" };
import { Access, AccessSchema, patterns } from "./access.ts";
import { authorize, ClaimsSchema, ScopeSchema, scopeAllows } from "./claims.ts";
import { GrantSchema } from "./contract.ts";
import { Key } from "./key.ts";
import { encodeGrants } from "./wire.ts";

test("shared bitrate wire vectors roundtrip without losing caps", () => {
	for (const row of vectors.valid) {
		const claims = ClaimsSchema.parse(row.input);
		expect(encodeGrants(claims)).toEqual(row.claims);
		const roundtrip = ClaimsSchema.parse(row.claims);
		expect(roundtrip.publish?.toJSON()).toEqual(claims.publish?.toJSON());
		expect(roundtrip.subscribe?.toJSON()).toEqual(claims.subscribe?.toJSON());
		expect(JSON.parse(JSON.stringify(GrantSchema.parse(row.input)))).toEqual(row.grant);
	}
	for (const input of vectors.invalid) {
		expect(ClaimsSchema.safeParse(input).success).toBe(false);
		expect(GrantSchema.safeParse(input).success).toBe(false);
	}
});

test("access refuses rates that are not positive safe integers", () => {
	const access = (rate?: number) => new Access({ path: patterns(["**"]), rate });
	for (const rate of [Number.NaN, Number.POSITIVE_INFINITY, 0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
		expect(() => access(rate)).toThrow();
		expect(AccessSchema.safeParse({ path: "**", rate }).success).toBe(false);
	}
});

test("caps survive signing and rebasing while key scopes refuse them", async () => {
	const input = {
		root: "room",
		publish: { path: "alice/**", rate: 8000 },
		subscribe: { path: "bob/**", rate: 16000 },
	};
	expect(ScopeSchema.safeParse(input).success).toBe(false);
	const key = await Key.generate("HS256");
	const claims = await Key.verify(key, await Key.sign(key, input));
	for (const path of ["/", "room", "room/alice"]) {
		const permissions = authorize(claims, path);
		expect(permissions.publish.rate).toBe(8000);
		expect(permissions.subscribe.rate).toBe(16000);
	}
	expect(scopeAllows(ScopeSchema.parse({ root: "room", publish: "**", subscribe: "**" }), claims)).toBe(true);
});
