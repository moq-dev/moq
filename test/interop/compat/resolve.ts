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

/** The command that re-measures planned breaks against the newest releases. */
const REMEASURE = "just test wire-compat --remeasure";

/** Refuse removed released versions unless a planned break covers them. */
export function matrix(currentAll: string[], releasedAll: string[], skipped: Record<string, PlannedBreak>) {
	const current = currentAll.filter(lite);
	const released = releasedAll.filter(lite);
	for (const [name, exception] of Object.entries(skipped)) {
		if (
			// The harness logs and records covering breaks as space-separated names.
			!/^[\w.-]+$/.test(name) ||
			!exception.reason?.trim() ||
			!Object.keys(exception.releases).length ||
			(exception.cells && !exception.cells.lanes.length)
		) {
			throw new Error(`invalid planned break: ${name}`);
		}
	}
	const missing = released.filter((v) => !current.includes(v) && !(skipped[v] && !skipped[v].cells));
	if (missing.length) throw new Error(`checkout removed published versions: ${missing.join(", ")}`);
	return {
		shared: released.filter((v) => current.includes(v)),
		currentOnly: current.filter((v) => !released.includes(v)),
		planned: skipped,
	};
}

/** Refuse a planned break measured against other releases, so a release forces its re-measure. */
export function stale(skipped: Record<string, PlannedBreak>, releasedAll: string[], releases: Record<string, string>) {
	const released = releasedAll.filter(lite);
	for (const [name, exception] of Object.entries(skipped)) {
		const versions = exception.cells ? (exception.cells.versions ?? []) : [name];
		const gone = versions.find((v) => !released.includes(v));
		if (gone) throw new Error(`stale planned break: ${name}, ${gone} is no longer released; run \`${REMEASURE}\``);
		for (const [pkg, expected] of Object.entries(exception.releases)) {
			if (releases[pkg] !== expected) {
				throw new Error(
					`stale planned break: ${name}, ${pkg} changed from ${expected} to ${releases[pkg]}; run \`${REMEASURE}\``,
				);
			}
		}
	}
}

/** One session cell planned breaks covered, run anyway by a re-measure. */
export interface Measured {
	names: string[];
	cell: string;
	passed: boolean;
}

/**
 * Re-pin each planned break that still fails to the measured releases, and delete the
 * rest, so every failing cell stays covered. A break that fails in a cell it covers
 * alone is kept. A failing cell several breaks cover keeps them all unless one is
 * already kept, since passing elsewhere cannot say which break it belongs to. A
 * protocol-keyed break also still fails while the checkout lacks its version.
 */
export function remeasure(
	skipped: Record<string, PlannedBreak>,
	measured: Measured[],
	{
		current,
		released: releasedAll,
		releases,
	}: { current: string[]; released: string[]; releases: Record<string, string> },
) {
	const released = releasedAll.filter(lite);
	const failed = measured.filter((m) => !m.passed);
	const kept = new Set(failed.filter((m) => m.names.length === 1).map((m) => m.names[0]));
	for (const m of failed) {
		if (!m.names.some((name) => kept.has(name))) for (const name of m.names) kept.add(name);
	}
	const breaks: Record<string, PlannedBreak> = {};
	const report: string[] = [];
	for (const [name, exception] of Object.entries(skipped)) {
		const covered = measured.filter((m) => m.names.includes(name));
		const failing = covered.filter((m) => !m.passed).map((m) => m.cell);
		const removed = !exception.cells && released.includes(name) && !current.includes(name);
		if (!kept.has(name) && !removed) {
			report.push(
				covered.length
					? `${name}: ${failing.length ? `${failing.length} of ${covered.length} cells fail, each covered by a kept break` : `no longer fails in ${covered.length} cells`}; deleted`
					: `${name}: covers no cells, deleted`,
			);
			continue;
		}
		const pins = Object.fromEntries(
			Object.keys(exception.releases).map((pkg) => {
				const version = releases[pkg];
				if (!version) throw new Error(`planned break ${name} pins ${pkg}, which the harness does not resolve`);
				return [pkg, version];
			}),
		);
		const entry: PlannedBreak = { ...exception, releases: pins };
		if (exception.cells?.versions) {
			entry.cells = {
				...exception.cells,
				versions: exception.cells.versions.filter((v) => released.includes(v)),
			};
		}
		breaks[name] = entry;
		report.push(
			removed
				? `${name}: still fails, the checkout no longer offers it; re-pinned`
				: `${name}: still fails in ${failing.length} of ${covered.length} cells; re-pinned\n  ${failing.join("\n  ")}`,
		);
	}
	return { breaks, report };
}

/** Build same-source media controls on the published lite-06 baseline. */
export function mediaControls(current: string[], released: string[]) {
	const version = "moq-lite-06";
	if (!current.includes(version) || !released.includes(version)) {
		throw new Error(`media controls require ${version} on both binaries`);
	}
	return [
		{ source: "current", version },
		{ source: "released", version },
	];
}

/** Every planned break that covers `cell`; a cell any break covers is skipped. */
export function planned(skipped: Record<string, PlannedBreak>, cell: Cell): string[] {
	if (cell.lane === "media-control") {
		if (cell.relay !== cell.publisher) throw new Error("media control must use one source");
		return [];
	}
	// A break keyed by protocol version covers every cell of that version.
	return Object.entries(skipped)
		.filter(([name, { cells }]) =>
			cells
				? cells.lanes.includes(cell.lane) &&
					(!cells.versions || cells.versions.includes(cell.version)) &&
					(!cells.relay || cells.relay === cell.relay) &&
					(!cells.publisher || cells.publisher === cell.publisher)
				: name === cell.version,
		)
		.map(([name]) => name);
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
		const [currentHelp, releasedHelp, breaks, shared, releases, controlsFile, mode] = args;
		const skipped = JSON.parse(readFileSync(breaks, "utf8"));
		const current = versions(readFileSync(currentHelp, "utf8"));
		const released = versions(readFileSync(releasedHelp, "utf8"));
		const result = matrix(current, released, skipped);
		// A re-measure runs every planned break against new releases, so it skips the pin check.
		if (mode !== "--remeasure") stale(skipped, released, JSON.parse(readFileSync(releases, "utf8")));
		const controls = mediaControls(current, released);
		console.log(JSON.stringify({ ...result, controls }, null, 2));
		writeFileSync(shared, `${result.shared.join("\n")}\n`);
		writeFileSync(controlsFile, `${controls.map(({ source, version }) => `${source} ${version}`).join("\n")}\n`);
	} else if (command === "remeasure") {
		// Rewrites the planned breaks in place, for the maintainer to review as a diff.
		const [breaks, currentHelp, releasedHelp, releases, measuredFile] = args;
		const measured = readFileSync(measuredFile, "utf8")
			.split("\n")
			.filter(Boolean)
			.map((line) => {
				const [names, result, cell] = line.split("\t");
				return { names: names.split(" "), cell, passed: result === "pass" };
			});
		const result = remeasure(JSON.parse(readFileSync(breaks, "utf8")), measured, {
			current: versions(readFileSync(currentHelp, "utf8")),
			released: versions(readFileSync(releasedHelp, "utf8")),
			releases: JSON.parse(readFileSync(releases, "utf8")),
		});
		writeFileSync(breaks, `${JSON.stringify(result.breaks, null, "\t")}\n`);
		console.log(result.report.join("\n"));
	} else if (command === "skip") {
		const [file, lane, version, relay, publisher] = args;
		const names = planned(JSON.parse(readFileSync(file, "utf8")), { lane, version, relay, publisher });
		if (names.length) console.log(names.join(" "));
	} else throw new Error("expected resolve, matrix, remeasure, or skip");
}
