import { readFileSync } from "node:fs";

/**
 * The scope a supported Rust CLI printed, each grant as prefixes. A released CLI prints
 * patterns: `p/**` is the prefix `p` and `**` the empty prefix, while a pattern without a
 * wildcard names one exact path, written here as `=p`.
 */
export function scope(text: string): [string | undefined, string[], string[]] {
	// The CLI's internal Access wrapper exposes the same permission paths.
	const compact = text.replace(/\s+/g, "");
	const grant = (field: string) => {
		const block = compact.match(new RegExp(`${field}:(?:Access\\{path:)?(Prefixes|Patterns)\\(\\[(.*?)\\]`));
		if (!block) throw new Error(`no ${field} in ${text}`);
		if (block[1] === "Prefixes") return [...block[2].matchAll(/"([^"]*)"/g)].map((match) => match[1]);
		return [...block[2].matchAll(/Pattern\("([^"]*)"\)/g)].map(([, pattern]) => {
			if (pattern === "**") return "";
			if (pattern.endsWith("/**") && !pattern.slice(0, -3).includes("*")) return pattern.slice(0, -3);
			if (pattern.includes("*")) throw new Error(`a wildcard grant: ${pattern}`);
			return `=${pattern}`;
		});
	};
	return [compact.match(/root:"([^"]*)"/)?.[1], grant("publish"), grant("subscribe")];
}

/**
 * Verify the scope every compat token grants: the `video` subtree to publish and everything to
 * subscribe, or with `read` "exact", the paths a released verifier reads a current token's
 * prefixes as.
 */
export function verify(text: string, read: "prefix" | "exact" = "prefix") {
	const expected = read === "exact" ? ["compat", ["=video"], ["="]] : ["compat", ["video"], [""]];
	const claims = scope(text);
	if (JSON.stringify(claims) !== JSON.stringify(expected)) {
		throw new Error(`token scope changed: ${JSON.stringify(claims)}`);
	}
}

if (import.meta.main) {
	const read = process.argv[3] ?? "prefix";
	if (read !== "prefix" && read !== "exact") throw new Error(`unknown read ${read}`);
	verify(readFileSync(process.argv[2], "utf8"), read);
}
