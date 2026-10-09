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

/** The matrix cells a planned break covers; an omitted field matches every cell. */
export interface Cells {
	lanes: string[];
	versions?: string[];
	relay?: "current" | "released";
	publisher?: "current" | "released";
}

/**
 * One reviewed exception, bound to the releases whose incompatibility was measured. Keyed
 * by protocol version it drops that version; with `cells` it skips only those cells.
 */
export interface PlannedBreak {
	reason: string;
	releases: Record<string, string>;
	cells?: Cells;
}

/** One session cell of the matrix. */
export interface Cell {
	lane: string;
	version: string;
	relay: string;
	publisher: string;
}

// moq-lite is what our clients and relays prefer with each other, so released-vs-current
// compares only lite. IETF drafts are for third-party interop, covered by `just test interop`.
const lite = (version: string) => version.startsWith("moq-lite-");

/** Refuse removed released versions unless a maintainer acknowledged the exact releases. */
export function matrix(
	currentAll: string[],
	releasedAll: string[],
	skipped: Record<string, PlannedBreak>,
	releases: Record<string, string> = {},
) {
	const current = currentAll.filter(lite);
	const released = releasedAll.filter(lite);
	for (const [name, exception] of Object.entries(skipped)) {
		const versions = exception.cells ? (exception.cells.versions ?? []) : [name];
		if (
			!exception.reason?.trim() ||
			!Object.keys(exception.releases).length ||
			(exception.cells && !exception.cells.lanes.length) ||
			versions.some((v) => !released.includes(v))
		) {
			throw new Error(`invalid or stale planned break: ${name}`);
		}
		for (const [pkg, expected] of Object.entries(exception.releases)) {
			if (releases[pkg] !== expected)
				throw new Error(`stale planned break: ${name}, ${pkg} changed from ${expected} to ${releases[pkg]}`);
		}
	}
	const dropped = (v: string) => !!skipped[v] && !skipped[v].cells;
	const missing = released.filter((v) => !current.includes(v) && !dropped(v));
	if (missing.length) throw new Error(`checkout removed published versions: ${missing.join(", ")}`);
	return {
		shared: released.filter((v) => current.includes(v) && !dropped(v)),
		currentOnly: current.filter((v) => !released.includes(v)),
		planned: skipped,
	};
}

/** The planned break that skips `cell`, if any. */
export function planned(skipped: Record<string, PlannedBreak>, cell: Cell): string | undefined {
	for (const [name, { cells }] of Object.entries(skipped)) {
		if (
			cells?.lanes.includes(cell.lane) &&
			(!cells.versions || cells.versions.includes(cell.version)) &&
			(!cells.relay || cells.relay === cell.relay) &&
			(!cells.publisher || cells.publisher === cell.publisher)
		) {
			return name;
		}
	}
	return undefined;
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
		// Every package the clients import directly, so no range drifts between nightlies.
		for (const name of ["@moq/net", "@moq/auth", "@moq/hang", "@moq/json", "@moq/web-transport"]) {
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
	} else if (command === "skip") {
		const [file, lane, version, relay, publisher] = args;
		const name = planned(JSON.parse(readFileSync(file, "utf8")), { lane, version, relay, publisher });
		if (name) console.log(name);
	} else throw new Error("expected resolve, matrix, or skip");
}
