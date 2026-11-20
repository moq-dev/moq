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
 * Verify the scope a compat token grants beneath `compat`: by default the `video` subtree to
 * publish and everything to subscribe, else `publish` and `subscribe`, each a comma-separated
 * list of prefixes. A verifier that read a prefix as one exact path fails it.
 */
export function verify(text: string, publish = "video", subscribe = "") {
	const expected = ["compat", publish.split(","), subscribe.split(",")];
	const claims = scope(text);
	if (JSON.stringify(claims) !== JSON.stringify(expected)) {
		throw new Error(`token scope changed: ${JSON.stringify(claims)}`);
	}
}

if (import.meta.main) {
	verify(readFileSync(process.argv[2], "utf8"), process.argv[3], process.argv[4]);
}
