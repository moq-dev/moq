import { readFileSync } from "node:fs";

/** Verify the permission scope printed by either supported Rust CLI. */
export function verify(text: string) {
	// The CLI's internal Access wrapper exposes the same permission paths.
	const compact = text.replace(/\s+/g, "");
	const patterns = (field: string) => {
		const block = compact.match(new RegExp(`${field}:(?:Access\\{path:)?Patterns\\(\\[(.*?)\\]`));
		if (!block) throw new Error(`no ${field} in ${text}`);
		return [...block[1].matchAll(/Pattern\("([^"]*)"\)/g)].map((match) => match[1]);
	};
	const claims = [compact.match(/root:"([^"]*)"/)?.[1], patterns("publish"), patterns("subscribe")];
	if (JSON.stringify(claims) !== JSON.stringify(["compat", ["video/**"], ["**"]])) {
		throw new Error(`token scope changed: ${JSON.stringify(claims)}`);
	}
}

if (import.meta.main) verify(readFileSync(process.argv[2], "utf8"));
