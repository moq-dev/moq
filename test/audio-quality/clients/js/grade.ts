/**
 * Grades a run's summaries against `budgets.json`, prints the table, and writes `grade.md`.
 *
 * Every budget value is a ceiling on one `<metric>_<aggregation>` key. Under `--enforce` a run fails
 * on any of: a value over its ceiling, a budgeted value the run never measured, a void row, or a row
 * with no budget at all. Each of those would otherwise read as a pass while grading nothing.
 *
 *     bun grade.ts --run <run dir> --budgets <budgets.json> [--enforce]
 *
 * @module
 */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { type Budgets, METRIC_KEYS, type Row, rowKey, type Summary } from "./src/schema.ts";

const { values } = parseArgs({
	options: {
		run: { type: "string" },
		budgets: { type: "string" },
		enforce: { type: "boolean", default: false },
	},
});
if (!values.run || !values.budgets) {
	console.error("usage: grade.ts --run <run dir> --budgets <budgets.json> [--enforce]");
	process.exit(2);
}
const run = values.run;

const budgets = JSON.parse(readFileSync(values.budgets, "utf8")) as Budgets;
const summaries = readdirSync(run)
	.filter((f) => f.endsWith(".summary.json"))
	.sort()
	.map((f) => JSON.parse(readFileSync(join(run, f), "utf8")) as Summary);
if (summaries.length === 0) {
	console.error(`error: no summaries in ${run}`);
	process.exit(1);
}

/** A budget applies to exactly one matrix cell, matched on the whole row. */
const budgetFor = (row: Row) =>
	budgets.rows.find(
		(b) =>
			b.runtime === row.runtime &&
			b.codec === row.codec &&
			b.rate === row.rate &&
			b.profile === row.profile &&
			b.ring === row.ring,
	);

const failures: string[] = [];
for (const summary of summaries) {
	const key = rowKey(summary.row);
	const budget = budgetFor(summary.row);
	if (!budget) {
		failures.push(`${key}: no budget`);
		continue;
	}
	for (const v of summary.voids) failures.push(`${key}: void, ${v.assertion} (${v.detail})`);
	if (summary.voids.length > 0) continue;
	for (const metric of METRIC_KEYS) {
		const ceiling = budget[metric];
		if (typeof ceiling !== "number") continue;
		const value = summary.metrics[metric];
		if (value === null || value === undefined) failures.push(`${key}: ${metric} unmeasured, ceiling ${ceiling}`);
		else if (value > ceiling) failures.push(`${key}: ${metric} ${value} > ${ceiling}`);
	}
}

/** The columns a reader scans first: what the listener would have noticed, and the delay paid for it. */
const COLUMNS: [string, string][] = [
	["underruns/min", "underruns_per_min"],
	["short/min", "short_quanta_per_min"],
	["episodes/min", "underrun_episodes_per_min"],
	["gap ms/min", "underrun_ms_per_min"],
	["skips/min", "skip_aheads_per_min"],
	["discarded ms/min", "discarded_ms_per_min"],
	["stalled", "stalled_share"],
	["silence", "silence_share"],
	["target p95", "target_ms_p95"],
	["converge ms", "converge_ms_last"],
];

const lines = [
	`| row | ${COLUMNS.map(([label]) => label).join(" | ")} | void |`,
	`|---|${COLUMNS.map(() => "---|").join("")}---|`,
	...summaries.map(
		(s) =>
			`| ${rowKey(s.row)} | ${COLUMNS.map(([, k]) => s.metrics[k] ?? "n/a").join(" | ")} | ${s.voids.map((v) => v.assertion).join(",") || "-"} |`,
	),
	"",
	...(failures.length > 0 ? ["failures:", ...failures.map((f) => `- ${f}`)] : ["within budget"]),
];
const report = lines.join("\n");
console.log(report);
await Bun.write(join(run, "grade.md"), `${report}\n`);

if (!values.enforce) {
	if (failures.length > 0) console.log("\nreporting only: pass --enforce to fail on these");
	process.exit(0);
}
process.exit(failures.length > 0 ? 1 : 0);
