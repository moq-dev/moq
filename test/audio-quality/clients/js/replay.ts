/**
 * Replays the recorded traces through the player's consumer and rings, and writes each row the way
 * `driver.ts` writes a browser row, so `analyze.ts` reduces both alike.
 *
 * The player half is `js/watch/src/audio/replay.ts`: the real container consumer and rings on a
 * simulated clock, at the delay a real `Sync` resolves for the recorded catalog and RTT. This half
 * reads its quanta through the same classifier the page's output tap runs, and samples the ring
 * every 250 ms of simulated time, as the probe samples a page. Every profile is the trace's name at
 * the "auto" delay a viewer gets by default.
 *
 *     bun replay.ts --traces ../../traces --profiles relay-bbb --rings plain --codecs aac --list
 *     bun replay.ts --traces ../../traces --out <run dir>
 *
 * `--list` prints the row tags; otherwise it writes `<tag>.ndjson` and `<tag>.page.json` per row.
 *
 * @module
 */
import { readdirSync, readFileSync } from "node:fs";
import { basename, join } from "node:path";
import { parseArgs } from "node:util";
import type * as Catalog from "@moq/hang/catalog";
import { classify, Ledger } from "./src/quantum.ts";
import {
	type Environment,
	type Ring,
	type Row,
	rowKey,
	SAMPLE_INTERVAL_MS,
	type Sample,
	SILENCE_RMS,
	type Stall,
	type Trace,
	traceCodec,
} from "./src/schema.ts";

const { values } = parseArgs({
	options: {
		traces: { type: "string" },
		profiles: { type: "string" },
		rings: { type: "string", default: "isolated,plain" },
		codecs: { type: "string", default: "opus,aac" },
		out: { type: "string" },
		list: { type: "boolean", default: false },
	},
});
if (!values.traces || (!values.list && !values.out)) {
	console.error("usage: replay.ts --traces DIR [--profiles a,b] [--rings r] [--codecs c] (--list | --out DIR)");
	process.exit(2);
}
const dir = values.traces;
const split = (list: string) => list.split(",").filter((s) => s !== "");

const traces = readdirSync(dir)
	.filter((f) => f.endsWith(".json"))
	.sort()
	.map((f) => ({ profile: basename(f, ".json"), trace: JSON.parse(readFileSync(join(dir, f), "utf8")) as Trace }));
const profiles = values.profiles === undefined ? undefined : split(values.profiles);
const rings = split(values.rings);
const codecs = split(values.codecs);
const selectors: [string, string[], string[]][] = [
	["profile", profiles ?? [], traces.map((t) => t.profile)],
	["ring", rings, ["isolated", "plain"]],
	["codec", codecs, ["opus", "aac"]],
];
for (const [name, list, known] of selectors) {
	const unknown = list.find((v) => !known.includes(v));
	if (unknown !== undefined) {
		console.error(`unknown ${name} '${unknown}' (known: ${known.join(", ")})`);
		process.exit(2);
	}
}

const rows: { row: Row; trace: Trace }[] = [];
for (const { profile, trace } of traces) {
	if (trace.version !== 1) throw new Error(`${profile}: trace version ${trace.version}, expected 1`);
	const codec = traceCodec(trace);
	if (profiles && !profiles.includes(profile)) continue;
	if (!codecs.includes(codec)) continue;
	for (const ring of rings as Ring[]) {
		rows.push({ row: { runtime: "replay", codec, rate: trace.config.sampleRate, profile, ring }, trace });
	}
}

if (values.list) {
	for (const { row } of rows) console.log(rowKey(row));
	process.exit(0);
}
const out = values.out as string;

// Loaded only to play: listing runs before `run.sh` has installed the workspace the player needs.
const { replay, target } = await import("../../../../js/watch/src/audio/replay.ts");

for (const { row, trace } of rows) {
	const tag = rowKey(row);
	const rate = row.rate;
	const delay = await target({
		delay: "auto",
		config: trace.config as unknown as Catalog.AudioConfig,
		rtt: trace.rtt ?? undefined,
	});

	const ledger = new Ledger(rate, SILENCE_RMS);
	const samples: Sample[] = [];
	let stalls: Stall[] = [];
	let stalled = true;
	let frame = 0;
	let next = SAMPLE_INTERVAL_MS;

	// The consumer's warnings, as the page's probe keeps a browser row's console.
	const notes: string[] = [];
	const warn = console.warn;
	console.warn = (...args: unknown[]) => {
		if (notes.length < 200) notes.push(args.map(String).join(" ").slice(0, 200));
	};

	const arrivals = trace.arrivals.map(([at, timestamp, group]) => ({ at, timestamp, group }));
	const quanta = replay(arrivals, { ring: row.ring === "isolated" ? "shared" : "post", rate, delay });
	for await (const quantum of quanta) {
		// The render clock is the simulated one: the trace's `at`, which starts at zero.
		ledger.add(frame, quantum.output.length, classify([quantum.output]));
		frame += quantum.output.length;
		if (quantum.stalled !== stalled) {
			stalled = quantum.stalled;
			stalls.push({ at: quantum.at, stalled });
		}

		if (quantum.at >= next) {
			next += SAMPLE_INTERVAL_MS;
			const gaps = ledger.take();
			samples.push({
				at: quantum.at,
				render: quantum.at,
				timestamp: quantum.timestamp,
				stalled: quantum.stalled,
				delay,
				quanta: ledger.counts.quanta,
				quiet: ledger.counts.quiet,
				gaps: gaps.length > 0 ? gaps : undefined,
				stalls: stalls.length > 0 ? stalls : undefined,
			});
			stalls = [];
		}
	}
	console.warn = warn;

	const environment: Environment = {
		crossOriginIsolated: row.ring === "isolated",
		transport: trace.transport,
		codec: trace.config.codec,
		rate,
		jitter: trace.config.jitter,
		contextRate: rate,
	};
	await Bun.write(join(out, `${tag}.ndjson`), `${samples.map((s) => JSON.stringify(s)).join("\n")}\n`);
	await Bun.write(join(out, `${tag}.page.json`), JSON.stringify({ environment, notes, voids: [] }, null, 1));
	console.log(`${tag}: ${samples.length} samples at a ${delay} ms target`);
}
