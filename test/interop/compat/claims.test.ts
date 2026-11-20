import { expect, test } from "bun:test";
import { verify } from "./claims";

// The current CLI's prefixes, and a released CLI's patterns for the same grant.
const prefixes =
	'publish: Access { path: Prefixes(["video",],), rate: None, }, subscribe: Access { path: Prefixes(["",],), rate: None, },';
const patterns = 'publish: Patterns([Pattern("video/**"),]), subscribe: Patterns([Pattern("**"),]),';
const access =
	'publish: Access { path: Patterns([Pattern("video/**"),]), rate: None, }, subscribe: Access { path: Patterns([Pattern("**"),]), rate: None, },';
const claims = (permissions: string) =>
	`Claims { root: "compat", ${permissions} expires: None, issued: None, not_before: None, }`;

for (const [name, permissions, changes] of [
	[
		"Prefixes",
		prefixes,
		[
			["video", "audio"],
			['[""', '["private"'],
		],
	],
	[
		"Patterns",
		patterns,
		[
			["video/**", "audio/**"],
			['Pattern("**")', 'Pattern("private/**")'],
		],
	],
	[
		"Access",
		access,
		[
			["video/**", "audio/**"],
			['Pattern("**")', 'Pattern("private/**")'],
		],
	],
] as const) {
	test(`verifies ${name} claims`, () => {
		expect(() => verify(claims(permissions))).not.toThrow();
		for (const [from, to] of [["compat", "other"], ...changes]) {
			expect(() => verify(claims(permissions).replace(from, to))).toThrow("token scope changed");
		}
	});
}

test("a released verifier reads a current token's prefixes as exact paths", () => {
	const exact = 'publish: Patterns([Pattern("video"),]), subscribe: Patterns([Pattern(""),]),';
	expect(() => verify(claims(exact), "exact")).not.toThrow();
	expect(() => verify(claims(exact))).toThrow("token scope changed");
	expect(() => verify(claims(patterns), "exact")).toThrow("token scope changed");
});

test("refuses a wildcard it cannot read as a prefix", () => {
	expect(() => verify(claims(patterns.replace("video/**", "*/video")))).toThrow("a wildcard grant");
});

test("refuses missing permission fields", () => {
	expect(() => verify('Claims { root: "compat" }')).toThrow("no publish");
});
