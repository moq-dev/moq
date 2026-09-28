/**
 * Reduces one row of a run directory to `<tag>.summary.json`, and prints it.
 *
 * Reads what the row left: `<tag>.ndjson` and `<tag>.page.json` from the driver, and the shaper's log
 * and exit status from run.sh.
 *
 *     bun analyze.ts --run <run dir> --row <tag> [--warmup 5]
 *
 * @module
 */
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { analyze } from "./src/analyze.ts";
import {
	type Environment,
	METRICS,
	parseRow,
	type Sample,
	type Shaper,
	type ShaperCounters,
	STAGES,
	type Void,
} from "./src/schema.ts";

const { values } = parseArgs({
	options: {
		run: { type: "string" },
		row: { type: "string" },
		warmup: { type: "string", default: "5" },
	},
});
if (!values.run || !values.row) {
	console.error("usage: analyze.ts --run <run dir> --row <tag> [--warmup 5]");
	process.exit(2);
}
const run = values.run;
const tag = values.row;
const read = (file: string) => {
	const path = join(run, file);
	return existsSync(path) ? readFileSync(path, "utf8") : undefined;
};

const samples: Sample[] = (read(`${tag}.ndjson`) ?? "")
	.split("\n")
	.filter((line) => line.trim() !== "")
	.map((line) => JSON.parse(line) as Sample);

const page = JSON.parse(read(`${tag}.page.json`) ?? "null") as {
	environment?: Environment;
	notes: string[];
	voids: Void[];
} | null;

/** The shaper prints its seed at start and its counters at exit; run.sh records the exit status. */
function shaperOf(): Shaper | null {
	const log = read(`shaper-${tag}.log`);
	const status = read(`shaper-${tag}.status`);
	if (log === undefined || status === undefined) return null;
	const counters = (direction: "up" | "down"): ShaperCounters | null => {
		const match = log.match(
			new RegExp(
				`${direction}: (\\d+) packets, (\\d+) lost, (\\d+) overflowed, (\\d+) throttled, (\\d+) delayed, (\\d+) reordered`,
			),
		);
		if (!match) return null;
		const [packets, lost, overflowed, throttled, delayed, reordered] = match.slice(1).map(Number) as number[];
		return { packets, lost, overflowed, throttled, delayed, reordered } as ShaperCounters;
	};
	const seed = log.match(/seed (\d+)/)?.[1];
	return {
		seed: seed === undefined ? null : Number(seed),
		status: Number.parseInt(status, 10),
		up: counters("up"),
		down: counters("down"),
	};
}

const summary = analyze({
	row: parseRow(tag),
	samples,
	environment: page?.environment,
	voids: page?.voids ?? [{ assertion: "driver", detail: "left no page report" }],
	notes: page?.notes ?? [],
	shaper: shaperOf(),
	warmupMs: Number.parseFloat(values.warmup) * 1000,
});
await Bun.write(join(run, `${tag}.summary.json`), JSON.stringify(summary, null, 1));

const lines = [`## ${tag}`, "", `window ${summary.windowMs} ms after a ${summary.warmupMs} ms warmup`, ""];
if (summary.voids.length > 0) {
	lines.push(`**void**: ${summary.voids.map((v) => `${v.assertion} (${v.detail})`).join("; ")}`, "");
}
lines.push("| metric | unit | clock | aggregation | value |", "|---|---|---|---|---|");
for (const [name, spec] of Object.entries(METRICS)) {
	for (const aggregation of spec.aggregations) {
		lines.push(
			`| ${name} | ${spec.unit} | ${spec.clock} | ${aggregation} | ${summary.metrics[`${name}_${aggregation}`] ?? "n/a"} |`,
		);
	}
}
lines.push(
	"",
	"| stage | ms | source |",
	"|---|---|---|",
	...STAGES.map((s) => `| ${s} | ${summary.stages[s].ms ?? "n/a"} | ${summary.stages[s].source} |`),
);
const shaper = summary.shaper;
if (shaper) {
	lines.push(
		"",
		`shaper seed ${shaper.seed} exit ${shaper.status}: up ${JSON.stringify(shaper.up)} down ${JSON.stringify(shaper.down)}`,
	);
}
if (summary.notes.length > 0) lines.push("", "notes:", ...summary.notes.slice(0, 20).map((n) => `- ${n}`));
console.log(lines.join("\n"));
