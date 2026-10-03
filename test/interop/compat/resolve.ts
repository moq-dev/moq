import { readFileSync, writeFileSync } from "node:fs";

/** Select registry releases that remain installable, excluding prereleases and yanked crates. */
export function newest(versions: { version: string; yanked?: boolean }[]): string {
	const candidates = versions.filter((v) => !v.yanked && /^\d+\.\d+\.\d+$/.test(v.version));
	candidates.sort((a, b) => Bun.semver.order(b.version, a.version));
	if (!candidates[0]) throw new Error("registry has no stable, non-yanked release");
	return candidates[0].version;
}

/** Read the connect-version choices without maintaining a draft list. */
export function versions(help: string): string[] {
	const start = help.indexOf("--connect-version");
	if (start < 0) throw new Error("CLI has no --connect-version");
	const choices = help.slice(start).match(/\[possible values: ([\s\S]*?)\]/)?.[1];
	if (!choices) throw new Error("CLI does not enumerate its supported versions");
	const values = choices.split(",").map((v) => v.trim());
	if (!values.length || values.some((v) => !/^moq-(lite|transport)-\d+(?:-wip)?$/.test(v))) {
		throw new Error(`malformed protocol choices: ${choices}`);
	}
	return values;
}

/** One reviewed exception, bound to the releases whose incompatibility was measured. */
export interface PlannedBreak {
	reason: string;
	releases: Record<string, string>;
}

/** Refuse removed released versions unless a maintainer acknowledged the exact releases. */
export function matrix(
	current: string[],
	released: string[],
	skipped: Record<string, PlannedBreak>,
	releases: Record<string, string> = {},
) {
	for (const [version, exception] of Object.entries(skipped)) {
		if (!released.includes(version) || !exception.reason?.trim() || !Object.keys(exception.releases).length) {
			throw new Error(`invalid or stale planned break: ${version}`);
		}
		for (const [name, expected] of Object.entries(exception.releases)) {
			if (releases[name] !== expected)
				throw new Error(
					`stale planned break: ${version}, ${name} changed from ${expected} to ${releases[name]}`,
				);
		}
	}
	const missing = released.filter((v) => !current.includes(v) && !skipped[v]);
	if (missing.length) throw new Error(`checkout removed published versions: ${missing.join(", ")}`);
	return {
		shared: released.filter((v) => current.includes(v) && !skipped[v]),
		currentOnly: current.filter((v) => !released.includes(v)),
		planned: skipped,
	};
}

async function registry(url: string) {
	const response = await fetch(url, {
		headers: { "User-Agent": "moq-wire-compat (https://github.com/moq-dev/moq)" },
	});
	if (!response.ok) throw new Error(`registry ${url}: ${response.status}`);
	return response.json();
}

if (import.meta.main) {
	const [command, ...args] = process.argv.slice(2);
	if (command === "resolve") {
		const result: Record<string, string> = {};
		for (const crate of ["moq-cli", "moq-relay", "hang"]) {
			const data = await registry(`https://crates.io/api/v1/crates/${crate}`);
			result[crate] = newest(
				data.versions.map((v: { num: string; yanked: boolean }) => ({ version: v.num, yanked: v.yanked })),
			);
		}
		for (const name of ["@moq/net", "@moq/auth", "@moq/hang"]) {
			const data = await registry(`https://registry.npmjs.org/${encodeURIComponent(name)}`);
			result[name] = newest(Object.keys(data.versions).map((version) => ({ version })));
		}
		writeFileSync(args[0], JSON.stringify(result, null, 2));
		console.log(JSON.stringify(result, null, 2));
	} else if (command === "matrix") {
		const skipped = JSON.parse(readFileSync(args[2], "utf8"));
		const result = matrix(
			versions(readFileSync(args[0], "utf8")),
			versions(readFileSync(args[1], "utf8")),
			skipped,
			JSON.parse(readFileSync(args[4], "utf8")),
		);
		console.log(JSON.stringify(result, null, 2));
		writeFileSync(args[3], `${result.shared.join("\n")}\n`);
	} else throw new Error("expected resolve or matrix");
}
