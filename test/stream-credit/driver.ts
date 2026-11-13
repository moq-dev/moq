/**
 * Drives the stream credit test: a JS publisher over WebSocket writes live groups on 300 tracks
 * to the relay, and a Chromium or Firefox tab reads all of them over WebTransport, whose
 * ~100-stream cap leaves the relay far short of the streams it wants to open.
 *
 *     bun driver.ts --port 4480 --version moq-lite-07-wip --engine chromium --out DIR --relay-log FILE [--timeout 60]
 *
 * Each track keeps one group open at a time, the way a live video track keeps its GOP open, so
 * the relay wants 300 streams to the tab at once. Tiers of subscriber priority decide who gets
 * them. Exits 0 when the top tier received every group whole, every subscription ended cleanly
 * soon after the publisher finished (on lite-07 with its final group whole), and the lower tiers
 * lost frames in priority order; 1 otherwise.
 *
 * @module
 */

import { readFileSync } from "node:fs";
import { parseArgs, stripVTControlCharacters } from "node:util";
import * as Moq from "@moq/net";
import { build } from "vite";
import { launch } from "../interop/clients/js/harness.ts";
import type { Group, Plan, Report, Track } from "./src/plan.ts";

const { values } = parseArgs({
	options: {
		port: { type: "string" },
		version: { type: "string" },
		engine: { type: "string" },
		out: { type: "string" },
		"relay-log": { type: "string" },
		timeout: { type: "string", default: "60" },
	},
});

const port = Number(values.port);
const version = values.version ?? "";
const engine = values.engine;
const out = values.out ?? "";
const pageDir = `${out}/page`;
const relayLog = values["relay-log"] ?? "";
const timeoutMs = Number(values.timeout) * 1000;
if (
	!Number.isInteger(port) ||
	!version ||
	(engine !== "chromium" && engine !== "firefox") ||
	!out ||
	!relayLog ||
	!(timeoutMs > 0)
) {
	console.error(
		"usage: driver.ts --port P --version V --engine chromium|firefox --out DIR --relay-log FILE [--timeout S]",
	);
	process.exit(2);
}

// Chromium and Firefox cap a WebTransport session at about 100 incoming streams. The top tier fits
// inside it with room to spare; the rest compete for what is left. Its readers want every group
// within a budget of a few group intervals, which only priority can meet under that contention:
// at one priority for all, its groups wait their turn and go stale. The rest read the live edge.
const TIERS = [
	{ name: "high", priority: 2, count: 30, budget: 500, every: true },
	{ name: "mid", priority: 1, count: 120, budget: 0, every: false },
	{ name: "low", priority: 0, count: 150, budget: 0, every: false },
] as const;
type Tier = (typeof TIERS)[number];

// Groups per track, one every interval, each with a frame at its start and one halfway through,
// so a group is open across the whole interval and a paused one has a frame left to continue.
const GROUPS = 40;
const INTERVAL_MS = 200;
// How long every subscription has to end once the publisher finishes. On loopback the relay
// drains the last groups in under a second; a subscription still open past this is stuck.
const DRAIN_MS = 10_000;

const broadcastName = "credit";
const relay = `http://127.0.0.1:${port}/`;

// @moq/net logs every finished publication at debug level; 300 of them bury the verdict.
console.debug = () => {};

function log(message: string) {
	console.error(`[stream-credit] ${message}`);
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const plan: Plan = {
	url: relay,
	version,
	broadcast: broadcastName,
	tracks: TIERS.flatMap((tier) =>
		Array.from({ length: tier.count }, (_, i) => ({
			name: `${tier.name}-${i}`,
			priority: tier.priority,
			budget: tier.budget,
		})),
	),
};
const tierOf = new Map<string, Tier>(
	TIERS.flatMap((tier) => plan.tracks.filter((t) => t.name.startsWith(`${tier.name}-`)).map((t) => [t.name, tier])),
);

const payload = (track: Track, sequence: number, frame: number) => `${track.name}:${sequence}:${frame}`;

// ── the publisher ────────────────────────────────────────────────────────────
const origin = new Moq.Origin.Producer();
const publication = origin.route(Moq.Path.from(broadcastName));
// No publisher age limit: each reader's budget alone decides which groups are still worth sending.
const tracks = plan.tracks.map((track) => publication.broadcast.createTrack(track.name, { maxAge: null }));
publication.announce();
// Bun has no WebTransport, so this dials the WebSocket, whose qmux stream limit @moq/net raises
// far past what 300 subscriptions need: the only cap in play is the browser's.
const publisher = await Moq.Connection.connect({ url: new URL(relay), versions: [version], publish: origin.consume() });
log(`publisher connected on ${publisher.version}`);

// ── the page ─────────────────────────────────────────────────────────────────
await build({
	configFile: false,
	root: import.meta.dir,
	logLevel: "warn",
	build: { target: "esnext", outDir: pageDir, emptyOutDir: true },
});
const server = Bun.serve({
	port: 0,
	hostname: "127.0.0.1",
	async fetch(req) {
		const path = new URL(req.url).pathname;
		const file = Bun.file(`${pageDir}${path === "/" ? "/index.html" : path}`);
		return (await file.exists()) ? new Response(file) : new Response("not found", { status: 404 });
	},
});

const browser = await launch([], engine);
// Every page log line, kept with the run: @moq/net logs each pause at debug level.
const pageLog = Bun.file(`${out}/console.log`).writer();
const failures: string[] = [];
let report: Report | undefined;
try {
	const page = await browser.newPage();
	page.on("pageerror", (error) => failures.push(`page error: ${error.message}`));
	page.on("console", (message) => {
		pageLog.write(`${performance.now().toFixed(0)} ${message.type()} ${message.text()}\n`);
		if (message.type() === "error") log(`page: ${message.text()}`);
	});
	await page.goto(`http://127.0.0.1:${server.port}/`);
	log(`${engine} ${browser.version()}`);

	const subscribed = page.evaluate((plan) => window.subscribe(plan), plan).then(() => true);
	if (!(await Promise.race([subscribed, sleep(timeoutMs).then(() => false)])))
		throw new Error(`the tab did not subscribe within ${timeoutMs} ms`);
	log(`tab subscribed to ${plan.tracks.length} tracks`);

	const reading = page.evaluate(() => window.report());
	// Left pending if the drain times out, and rejected once the browser closes.
	reading.catch(() => {});
	// Every track keeps one group open: the previous one finishes as the next one starts.
	let open: Moq.Group.Producer[] = [];
	const write = (sequence: number, frame: number) => {
		for (const [i, group] of open.entries())
			group.writeFrame({ payload: new TextEncoder().encode(payload(plan.tracks[i], sequence, frame)) });
	};
	for (let sequence = 0; sequence < GROUPS; sequence++) {
		for (const group of open) group.finish();
		open = tracks.map((track) => track.appendGroup());
		write(sequence, 0);
		await sleep(INTERVAL_MS / 2);
		write(sequence, 1);
		await sleep(INTERVAL_MS / 2);
	}
	for (const group of open) group.finish();
	for (const track of tracks) track.finish();
	log(`published ${GROUPS} groups on each track`);

	const drained = await Promise.race([reading, sleep(DRAIN_MS).then(() => undefined)]);
	if (drained) {
		report = drained;
	} else {
		const pending = await page.evaluate(() => window.pending());
		failures.push(`${pending.length} subscriptions still open ${DRAIN_MS} ms after the publisher finished`);
		for (const received of pending.slice(0, 5)) {
			const groups = received.groups
				.slice(-4)
				.map((g) => `${g.sequence}:${g.frames.length}${g.error ? `:${g.error}` : ""}`);
			failures.push(`${received.name} is still open; last groups ${groups.join(" ")}`);
		}
		await Bun.write(`${out}/pending.json`, JSON.stringify(pending));
	}
} catch (err) {
	failures.push(err instanceof Error ? err.message : String(err));
} finally {
	await browser.close().catch(() => {});
	await pageLog.end();
	server.stop(true);
	await publisher.close();
	origin.close();
}

if (report) {
	await Bun.write(`${out}/report.json`, JSON.stringify(report));
	judge(report);
}

function judge(report: Report) {
	if (report.version !== version) failures.push(`the tab negotiated ${report.version}, not ${version}`);
	if (report.closed) failures.push(`the tab's session closed with ${report.closed}`);
	const accounted = !/^moq-lite-0[0-6]$/.test(version);

	// Frames read per tier, the measure of who the streams went to: a live-edge reader's group
	// goes stale as its successor starts, so most of its groups end early by design.
	const frames = new Map<Tier, number>(TIERS.map((tier) => [tier, 0]));
	for (const received of report.tracks) {
		const track = plan.tracks.find((t) => t.name === received.name);
		const tier = tierOf.get(received.name);
		if (!track || !tier) throw new Error(`unknown track ${received.name}`);
		const fail = (detail: string) => failures.push(`${received.name}: ${detail}`);

		if (received.ended) fail(`ended with ${received.ended}`);
		const seen = new Set<number>();
		let last: Group | undefined;
		for (const group of received.groups) {
			if (seen.has(group.sequence)) fail(`group ${group.sequence} handed out twice`);
			seen.add(group.sequence);
			if (group.sequence === GROUPS - 1) last = group;
			// A frame read is never wrong, only missing: each one read is the next one written.
			const expected = [0, 1].map((frame) => payload(track, group.sequence, frame));
			if (group.frames.some((frame, i) => frame !== expected[i]))
				fail(`group ${group.sequence} read ${JSON.stringify(group.frames)}`);
			frames.set(tier, (frames.get(tier) ?? 0) + group.frames.length);
			// The top tier asks for every group, so none may stop early.
			if (tier.every && (group.error || group.frames.length !== expected.length))
				fail(`group ${group.sequence} stopped after ${group.frames.length} frames: ${group.error}`);
		}
		if (tier.every) {
			const missing = Array.from({ length: GROUPS }, (_, i) => i).filter((i) => !seen.has(i));
			if (missing.length) fail(`missed groups ${missing.join(",")}`);
		}
		// Once the publisher stops, the backlog drains: the newest group reaches every reader whole.
		// Only lite-07 accounts for every group, so only there does a reader wait for one still
		// queued for credit at the track's end. A lite-06 reader cannot tell a group still coming
		// from one the relay shed, so it ends at the end it was told, and a starved track loses
		// the final group the relay had not opened yet.
		if ((accounted || tier.every) && (!last || last.error || last.frames.length !== 2))
			fail(`final group ${last ? `stopped after ${last.frames.length} frames: ${last.error}` : "never arrived"}`);
	}

	const share = TIERS.map((tier) => {
		const tracks = report.tracks.filter((t) => tierOf.get(t.name) === tier);
		const ratio = (frames.get(tier) ?? 0) / (tracks.length * GROUPS * 2);
		const slowest = tracks.reduce((a, b) => ((b.at ?? 0) > (a.at ?? 0) ? b : a));
		const ended = slowest.at ?? 0;
		log(
			`${tier.name} (priority ${tier.priority}, ${tracks.length} tracks): read ${(ratio * 100).toFixed(1)}% of frames, all ended ${ended.toFixed(0)} ms after subscribing (${slowest.name})`,
		);
		return ratio;
	});
	// More live groups than the cap must cost something, and it must fall on the lowest priority.
	const [high, mid, low] = share;
	if (!(low < 1)) failures.push("the low tier read every frame: the browser's stream cap never bound");
	if (!(high >= mid && mid > low)) failures.push(`frames read did not follow priority: ${share.join(", ")}`);

	// The relay's own account of its scheduling, independent of what the tab read: the tab's
	// session is the last WebTransport one negotiated on this version.
	const lines = stripVTControlCharacters(readFileSync(relayLog, "utf8")).split("\n");
	const negotiated = new RegExp(`conn\\{id=(\\d+) .*negotiated version=${version} transport=webtransport`);
	const id = lines.flatMap((line) => negotiated.exec(line)?.[1] ?? []).at(-1);
	if (id === undefined) throw new Error(`the relay log names no WebTransport session on ${version}`);
	const ours = lines.filter((line) => line.includes(`conn{id=${id} `));
	const count = (what: string) => ours.filter((line) => line.includes(what)).length;
	const continued = count("continuing paused group");
	log(`relay: ${continued} paused groups continued`);
	// Lite-07's YIELDED reset pauses a group to make room, and a later stream continues it.
	if (accounted && continued === 0) failures.push("the relay never continued a paused group");
}

if (failures.length) {
	for (const failure of failures.slice(0, 50)) console.error(`FAIL ${failure}`);
	if (failures.length > 50) console.error(`... and ${failures.length - 50} more`);
	process.exit(1);
}
console.log(`PASS ${plan.tracks.length} tracks through the browser's stream cap on ${version}`);
process.exit(0);
