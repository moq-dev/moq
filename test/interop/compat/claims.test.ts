import { expect, test } from "bun:test";
import { verify } from "./claims";

const patterns = 'publish: Patterns([Pattern("video/**"),]), subscribe: Patterns([Pattern("**"),]),';
const access =
	'publish: Access { path: Patterns([Pattern("video/**"),]), rate: None, }, subscribe: Access { path: Patterns([Pattern("**"),]), rate: None, },';

for (const permissions of [patterns, access]) {
	const claims = `Claims { root: "compat", ${permissions} expires: None, issued: None, not_before: None, }`;
	test(`verifies ${permissions.startsWith("publish: Access") ? "Access" : "Patterns"} claims`, () => {
		expect(() => verify(claims)).not.toThrow();
		for (const [from, to] of [
			["compat", "other"],
			["video/**", "audio/**"],
			['Pattern("**")', 'Pattern("private/**")'],
		]) {
			expect(() => verify(claims.replace(from, to))).toThrow("token scope changed");
		}
	});
}

test("refuses missing permission fields", () => {
	expect(() => verify('Claims { root: "compat" }')).toThrow("no publish");
});
