/**
 * Runs the browser benchmark scenarios against one relay and retains every raw sample.
 *
 * Variants alternate within each round, so host drift lands on every variant alike. Timed,
 * traced and counted passes stay separate, so tracing and copy counting never inflate a timing.
 * Correctness fails the run; timing is reported, never judged.
 */
import { appendFile } from "node:fs/promises";
import { cpus, freemem, loadavg, platform, totalmem } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { type Browser, type CDPSession, chromium, type Page } from "playwright";
import { install as countCalls, withoutGetStats, withoutWebTransport } from "./calls.ts";
import { install, read } from "./counters.ts";
import { check, digest, payload } from "./fixture.ts";
import type { Cmaf, Phase } from "./page/cmaf.ts";
import type { Media } from "./page/media.ts";
import type { Counts, Outcome, Sampling } from "./page/stats.ts";
import type { Delivery, Transport } from "./page/transport.ts";
import { compare, type Row, summarize } from "./summary.ts";

const { values } = parseArgs({
	options: {
		relay: { type: "string" },
		dist: { type: "string" },
		out: { type: "string" },
		variants: { type: "string" },
		rounds: { type: "string", default: "5" },
		warmup: { type: "string", default: "1" },
		scenarios: { type: "string", default: "transport,cmaf,media,stats" },
		smoke: { type: "boolean", default: false },
		timeout: { type: "string", default: "60" },
	},
});
const integer = (name: string, value: string | undefined, min: number) => {
	const parsed = Number(value);
	if (!Number.isInteger(parsed) || parsed < min) throw new Error(`--${name} must be an integer >= ${min}`);
	return parsed;
};
if (!values.relay || !values.dist || !values.out || !values.variants)
	throw new Error("driver needs --relay, --dist, --out and --variants");
const relay = values.relay;
const dist = values.dist;
const out = values.out;
const variants = values.variants.split(",");
if (!variants.includes("current")) throw new Error("--variants must include current");
// The change under test is current against base; every control is judged against current.
const pairs: [string, string][] = variants
	.filter((variant) => variant !== "current")
	.map((variant) => (variant === "base" ? ["current", "base"] : [variant, "current"]));
const smoke = values.smoke;
const rounds = smoke ? 1 : integer("rounds", values.rounds, 1);
const warmup = smoke ? 0 : integer("warmup", values.warmup, 0);
const timeout = integer("timeout", values.timeout, 1) * 1000;
const scenarios = new Set(values.scenarios.split(","));
for (const scenario of scenarios)
	if (!["transport", "cmaf", "media", "stats"].includes(scenario)) throw new Error(`unknown scenario: ${scenario}`);

// Fixed so a run on another host or day compares like with like. Software rendering and decode:
// a hardware path appearing or disappearing would change what is measured.
const FLAGS = [
	"--disable-gpu",
	"--disable-background-timer-throttling",
	"--disable-renderer-backgrounding",
	"--disable-backgrounding-occluded-windows",
	"--autoplay-policy=no-user-gesture-required",
];
// Shipping Chromium has no WebTransport getStats(); the stats scenario runs in a second browser
// exposing it, so sampling does the transport work a browser with the API does. Its `absent`
// shapes delete it again, which is what shipping Chromium and Safari present.
const STATS_FLAGS = [...FLAGS, "--enable-blink-features=WebTransportStats"];

// The costs worth reading at a glance; report.json keeps every metric.
const HEADLINE = new Set([
	"consumer_main_ms",
	"consumer_ms",
	"network_ms",
	"main_ms",
	"watcher_ms_per_s",
	"publisher_ms_per_s",
	"presented_fps",
	"getstats_per_s",
	"main_ms_per_s",
	"first_estimate_p50_ms",
]);
// Deterministic or one-off metrics from the traced, counted and probed passes.
const INSTRUMENTED = /^(copied|allocated|gc_|frame_delay)/;

const TRANSPORT = smoke
	? [
			{ size: 160, frames: 200, group: 10 },
			{ size: 131072, frames: 16, group: 8 },
		]
	: [
			{ size: 160, frames: 3000, group: 10 },
			{ size: 1200, frames: 2000, group: 10 },
			{ size: 16384, frames: 800, group: 30 },
			{ size: 131072, frames: 160, group: 30 },
			{ size: 1048576, frames: 24, group: 8 },
		];
const CMAF: Cmaf[] = [
	{ kind: "audio", sizes: Array(50).fill(160), iterations: smoke ? 2 : 200 },
	{ kind: "video", sizes: [65536, ...Array(29).fill(4096)], iterations: smoke ? 2 : 100 },
];
const MEDIA = { width: 1280, height: 720, fps: 30, codec: "avc1", bitrate: 2_500_000, keyframe_ms: 1000 };
const MEDIA_WARMUP_MS = smoke ? 1000 : 3000;
const MEDIA_WINDOW_MS = smoke ? 3000 : 10000;
/** A sampling shape; `absent` deletes WebTransport getStats(), as shipping Chromium lacks it. */
type SamplingShape = Omit<Sampling, "url" | "path"> & { absent?: boolean };
const grid = (mode: Sampling["mode"], connections: number[], reservations: number[], absent?: boolean) =>
	connections.flatMap((c) => reservations.map((r): SamplingShape => ({ mode, connections: c, reservations: r, absent })));
// Distinct connections crossed with reservations held on each, plus receive-only playback,
// pool linger, identical pooled handles (one connection, the control), qmux, and private
// loops on a WebTransport without getStats().
const SAMPLING: SamplingShape[] = smoke
	? [
			...grid("private", [1, 4], [0, 1]),
			...grid("receive", [1], [0]),
			...grid("linger", [1], [0]),
			...grid("pooled", [4], [0]),
			...grid("qmux", [1], [0, 1]),
			...grid("private", [1], [0, 1], true),
		]
	: [
			...grid("private", [1, 4, 16], [0, 1, 4]),
			...grid("receive", [1, 16], [0]),
			...grid("linger", [1, 16], [0]),
			...grid("pooled", [4], [0, 1]),
			...grid("qmux", [4], [0, 1]),
			...grid("private", [1, 16], [0, 1], true),
		];
// Each shape settles before its window, so connection setup stays out of it. Linger's window
// must close inside the pool's two-second linger.
const SAMPLING_SETTLE_MS = 500;
const SAMPLING_WINDOW_MS = smoke ? 1000 : 2000;
const LINGER_WINDOW_MS = 1200;
const RESUME = { cycles: smoke ? 3 : 10, idle_ms: 500, limit_ms: 2000 };

const server = Bun.serve({
	hostname: "127.0.0.1",
	port: 0,
	async fetch(request) {
		const path = new URL(request.url).pathname;
		if (path === "/cmaf-fixtures.json")
			return new Response(Bun.file(new URL("../../js/hang/src/cmaf/fixtures.json", import.meta.url)));
		const page = /^\/([a-z0-9-]+)\/(transport|cmaf|media|stats)$/.exec(path);
		if (page)
			return new Response(
				`<!doctype html><body><script type="module" src="/${page[1]}/${page[2]}.js"></script>`,
				{
					headers: { "content-type": "text/html", "cache-control": "no-store" },
				},
			);
		const file = Bun.file(join(dist, path));
		return (await file.exists())
			? new Response(file, { headers: { "cache-control": "no-store" } })
			: new Response("not found", { status: 404 });
	},
});
const origin = `http://127.0.0.1:${server.port}`;

const report = {
	version: 1,
	smoke,
	relay,
	pairs,
	variants,
	revisions: JSON.parse(process.env.BENCH_REVISIONS ?? "{}"),
	rounds,
	warmup,
	browser: {
		executable: process.env.BROWSER_BIN ?? "playwright chromium",
		version: "",
		flags: FLAGS,
		stats_flags: STATS_FLAGS,
		page: {},
	},
	host: {
		platform: platform(),
		cpu: cpus()[0]?.model,
		logical_cpus: cpus().length,
		memory: { total: totalmem(), free: freemem() },
		load: loadavg(),
	},
	limits: [
		"Main-thread CPU is CDP TaskDuration in threadTicks. Process CPU is SystemInfo cpuTime, which includes every thread of the page's renderer (decode, compositing, GC helpers).",
		"Network CPU is the browser's network service process, shared by every page; WebTransport framing and QUIC run there.",
		"GC pauses and reclaimed JS heap come from a separate traced pass; tracing overhead never enters a timed sample.",
		"Copied and allocated bytes count JS-visible typed-array copies only, from a separate counted pass. Native copies (WebTransport, WebCodecs) are unavailable.",
		"Software decode and rendering (--disable-gpu). Hardware paths are not measured.",
		"Frame delay joins publisher paint time with watcher presentation time across pages on one host clock; only the probed pass reads frame ids back.",
	],
	samples: [] as Row[],
	summary: [] as unknown[],
	comparisons: [] as unknown[],
};
const output = join(out, "report.json");
// Writes queue behind each other and serialize when they run, so the newest snapshot always lands last.
let saving: Promise<unknown> = Promise.resolve();
const save = () => {
	saving = saving.then(() => Bun.write(output, JSON.stringify(report, null, 2)));
	return saving;
};

interface Chromium {
	browser: Browser;
	system: CDPSession;
}

async function launch(args: string[]): Promise<Chromium> {
	const browser = await chromium.launch({
		channel: process.env.BROWSER_BIN ? undefined : "chromium",
		executablePath: process.env.BROWSER_BIN,
		headless: true,
		args,
	});
	return { browser, system: await browser.newBrowserCDPSession() };
}

const main = await launch(FLAGS);
const { browser } = main;
report.browser.version = browser.version();
const exposed = scenarios.has("stats") ? await launch(STATS_FLAGS) : undefined;

try {
	for (let round = 0; round < warmup + rounds; round++) {
		// Rotating the start keeps any variant from always running first or last.
		const order = variants.map((_, index) => variants[(index + round) % variants.length]);
		const label = round < warmup ? "warmup" : `round ${round - warmup + 1}/${rounds}`;
		console.log(`── ${label}: ${order.join(", ")}`);
		if (scenarios.has("transport"))
			for (const shape of TRANSPORT)
				for (const variant of order) await sample(transport(variant, shape, round, "timed"));
		if (scenarios.has("cmaf"))
			for (const shape of CMAF) for (const variant of order) await sample(cmaf(variant, shape, round, "timed"));
		if (scenarios.has("media")) for (const variant of order) await sample(media(variant, round, "timed"));
		if (scenarios.has("stats")) {
			// Nothing here receives stream chunks in bulk, so the copy control measures nothing.
			const measured = order.filter((variant) => variant !== "copy");
			for (const shape of SAMPLING)
				for (const variant of measured) await sample(sampling(variant, shape, round, "timed"));
			for (const variant of measured) await sample(resume(variant, round));
		}
	}
	console.log("── traced and counted passes");
	for (const variant of variants) {
		if (scenarios.has("transport"))
			for (const shape of TRANSPORT) {
				await sample(transport(variant, shape, -1, "traced"));
				await sample(transport(variant, shape, -1, "counted"));
			}
		if (scenarios.has("cmaf")) for (const shape of CMAF) await sample(cmaf(variant, shape, -1, "traced"));
		if (scenarios.has("media")) {
			await sample(media(variant, -1, "traced"));
			await sample(media(variant, -1, "probed"));
		}
		if (scenarios.has("stats") && variant !== "copy")
			for (const shape of SAMPLING) {
				await sample(sampling(variant, shape, -1, "traced"));
				await sample(sampling(variant, shape, -1, "counted"));
			}
	}
} finally {
	await browser.close();
	await exposed?.browser.close();
	server.stop(true);
	const timed = report.samples.filter((row) => row.pass === "timed" && row.round >= warmup);
	report.summary = summarize(timed);
	report.comparisons = compare(timed, pairs);
	await save();
}
console.log(`Retained ${report.samples.length} samples: ${output}`);
await printComparisons();

type Pass = "timed" | "traced" | "counted" | "probed";

interface Opened {
	page: Page;
	cdp: CDPSession;
	chromium: Chromium;
	pid: number;
	errors: string[];
	close: () => Promise<void>;
}

async function open(
	variant: string,
	kind: string,
	counted = false,
	init: (() => void)[] = [],
	on: Chromium = main,
): Promise<Opened> {
	const context = await on.browser.newContext({ viewport: { width: 1280, height: 720 } });
	const page = await context.newPage();
	const errors: string[] = [];
	page.on("pageerror", (error) => errors.push(error.message));
	if (counted) await page.addInitScript(install);
	for (const script of init) await page.addInitScript(script);
	await page.goto(`${origin}/${variant}/${kind}`);
	await page.waitForFunction((kind) => kind in window && "environment" in window, kind);
	const environment = await page.evaluate(() => window.environment());
	// A hidden page throttles timers and rendering, which invalidates every comparison.
	if (environment.visibility !== "visible") throw new Error(`${variant}/${kind} page is ${environment.visibility}`);
	(report.browser.page as Record<string, unknown>)[kind] = environment;
	const cdp = await context.newCDPSession(page);
	// Refuse an unavailable CPU clock rather than relabeling wall time as CPU.
	await cdp.send("Performance.enable", { timeDomain: "threadTicks" });
	return { page, cdp, chromium: on, pid: await rendererPid(page, on.browser), errors, close: () => context.close() };
}

// The renderer process behind a page, from the frame list a trace starts with.
async function rendererPid(page: Page, browser: Browser): Promise<number> {
	await browser.startTracing(page, { categories: ["disabled-by-default-devtools.timeline"] });
	const events = traceEvents(await browser.stopTracing());
	const url = page.url();
	for (const event of events) {
		if (event.name !== "TracingStartedInBrowser") continue;
		const frames = (event.args?.data?.frames ?? []) as { url: string; processId: number }[];
		const frame = frames.find((frame) => frame.url === url);
		if (frame) return frame.processId;
	}
	throw new Error(`no renderer process found for ${url}`);
}

interface TraceEvent {
	name: string;
	ph: string;
	pid: number;
	tid: number;
	dur?: number;
	args?: { data?: { frames?: unknown }; usedHeapSizeBefore?: number; usedHeapSizeAfter?: number };
}

function traceEvents(buffer: Buffer): TraceEvent[] {
	const trace = JSON.parse(buffer.toString()) as { traceEvents?: TraceEvent[] } | TraceEvent[];
	return Array.isArray(trace) ? trace : (trace.traceEvents ?? []);
}

// GC on one renderer's main thread: total pause, count, and the JS heap each collection reclaimed.
function collections(events: TraceEvent[], pid: number) {
	const gcs = events.filter(
		(event) => event.pid === pid && event.tid === pid && (event.name === "MinorGC" || event.name === "MajorGC"),
	);
	const reclaimed = gcs.reduce(
		(sum, event) => sum + Math.max(0, (event.args?.usedHeapSizeBefore ?? 0) - (event.args?.usedHeapSizeAfter ?? 0)),
		0,
	);
	return {
		count: gcs.length,
		minor: gcs.filter((event) => event.name === "MinorGC").length,
		pause_ms: gcs.reduce((sum, event) => sum + (event.dur ?? 0), 0) / 1000,
		reclaimed_bytes: reclaimed,
	};
}

async function metrics(opened: Opened) {
	const { metrics } = await opened.cdp.send("Performance.getMetrics");
	const value = (name: string) => metrics.find((metric) => metric.name === name)?.value;
	const task = value("TaskDuration");
	if (task === undefined || !Number.isFinite(task)) throw new Error("invalid main-thread CPU clock");
	return { main_s: task, heap: value("JSHeapUsedSize") ?? null };
}

async function processes(system = main.system) {
	const { processInfo } = (await system.send("SystemInfo.getProcessInfo")) as {
		processInfo: { type: string; id: number; cpuTime: number }[];
	};
	return processInfo;
}

// Process CPU between two snapshots: each page's renderer, plus the shared network, GPU and browser processes.
function processCpu(
	before: Awaited<ReturnType<typeof processes>>,
	after: Awaited<ReturnType<typeof processes>>,
	pages: Record<string, number>,
) {
	const delta = (match: (process: { type: string; id: number }) => boolean) => {
		let sum = 0;
		for (const process of after.filter(match)) {
			const previous = before.find((entry) => entry.id === process.id);
			sum += process.cpuTime - (previous?.cpuTime ?? 0);
		}
		return sum * 1000;
	};
	return {
		...Object.fromEntries(Object.entries(pages).map(([role, pid]) => [`${role}_ms`, delta((p) => p.id === pid)])),
		network_ms: delta((p) => p.type.includes("NetworkService")),
		gpu_ms: delta((p) => p.type === "GPU"),
		browser_ms: delta((p) => p.type === "browser"),
	};
}

async function bounded<T>(task: Promise<T>, what: string, limit = timeout): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			task,
			new Promise<never>((_, reject) => {
				timer = setTimeout(() => reject(new Error(`${what} did not finish within ${limit / 1000}s`)), limit);
			}),
		]);
	} finally {
		clearTimeout(timer);
	}
}

// Every step of a sample is bounded, but a renderer stuck in synchronous code also stalls the
// CDP and evaluate calls between them. Bounding the whole sample fails the run instead, and the
// outer finally closes the browser.
function sample(task: Promise<void>): Promise<void> {
	return bounded(task, "a sample", 4 * timeout + MEDIA_WARMUP_MS + MEDIA_WINDOW_MS);
}

function record(row: Row) {
	report.samples.push(row);
	void save();
}

// Runs `run`, tracing it into a retained file only on the traced pass.
async function trace(
	pass: Pass,
	page: Page,
	name: string,
	run: () => Promise<void>,
	browser = main.browser,
): Promise<TraceEvent[] | undefined> {
	if (pass !== "traced") {
		await run();
		return undefined;
	}
	await browser.startTracing(page, {
		categories: ["devtools.timeline", "v8", "disabled-by-default-v8.gc", "disabled-by-default-devtools.timeline"],
	});
	let buffer: Buffer | undefined;
	try {
		await run();
	} finally {
		buffer = await browser.stopTracing();
	}
	// Compressed: a media trace runs to tens of megabytes.
	await Bun.write(join(out, "traces", `${name}.json.gz`), Bun.gzipSync(new Uint8Array(buffer)));
	return traceEvents(buffer);
}

async function transport(
	variant: string,
	shape: { size: number; frames: number; group: number },
	round: number,
	pass: Pass,
) {
	const config: Transport = {
		url: relay,
		path: `transport/${crypto.randomUUID()}`,
		...shape,
		full: pass === "counted",
	};
	const key = `transport ${shape.size}B x${shape.frames}`;
	const publisher = await open(variant, "transport");
	const consumer = await open(variant, "transport", pass === "counted");
	try {
		await bounded(
			publisher.page.evaluate((config) => window.transport.publish(config), config),
			"publisher setup",
		);
		const version = await bounded(
			consumer.page.evaluate((config) => window.transport.prepare(config), config),
			"consumer setup",
		);
		// Setup copies (bundle start, connect, SUBSCRIBE plumbing) are not the stream path's.
		const setup = pass === "counted" ? await consumer.page.evaluate(read) : undefined;
		const before = {
			consumer: await metrics(consumer),
			publisher: await metrics(publisher),
			processes: await processes(),
		};
		let delivery: Delivery | undefined;
		const consume = async () => {
			delivery = await bounded(
				consumer.page.evaluate((config) => window.transport.consume(config), config),
				`${key} delivery`,
			);
		};
		const events = await trace(pass, consumer.page, `${key.replaceAll(" ", "-")}-${variant}`, consume);
		const after = {
			consumer: await metrics(consumer),
			publisher: await metrics(publisher),
			processes: await processes(),
		};
		const errors = [...publisher.errors, ...consumer.errors];
		if (errors.length) throw new Error(errors.join("; "));
		if (!delivery) throw new Error("no delivery");
		// Counts and the digest prove every frame arrived intact, so skipped work cannot read as a speedup.
		const expected = digest(shape.size, shape.frames);
		if (
			delivery.frames !== shape.frames ||
			delivery.bytes !== shape.size * shape.frames ||
			delivery.digest !== expected
		)
			throw new Error(
				`${key} ${variant}: delivered ${delivery.frames} frames, digest ${delivery.digest} != ${expected}`,
			);
		const total = setup && (await consumer.page.evaluate(read));
		const counts = setup &&
			total && { copied: total.copied - setup.copied, allocated: total.allocated - setup.allocated };
		const row: Row = {
			scenario: "transport",
			key,
			shape: { ...shape, protocol: version },
			variant,
			round,
			pass,
			delivered: { frames: delivery.frames, bytes: delivery.bytes, digest: delivery.digest },
			metrics: {
				wall_ms: delivery.elapsed_ms,
				consumer_main_ms: (after.consumer.main_s - before.consumer.main_s) * 1000,
				publisher_main_ms: (after.publisher.main_s - before.publisher.main_s) * 1000,
				...processCpu(before.processes, after.processes, { consumer: consumer.pid, publisher: publisher.pid }),
				long_task_ms: delivery.long_tasks.total_ms,
				...(events && gcMetrics(collections(events, consumer.pid))),
				...(counts && {
					copied_per_byte: counts.copied / delivery.bytes,
					allocated_per_byte: counts.allocated / delivery.bytes,
				}),
			},
			heap: { before: before.consumer.heap, after: after.consumer.heap },
			load: loadavg(),
		};
		record(row);
		log(row);
		// Page-side cleanup only after success: a page stuck in synchronous code would never
		// answer it, while closing the context is handled by the browser process.
		await publisher.page.evaluate(() => window.transport.stop());
		await consumer.page.evaluate(() => window.transport.stop());
	} finally {
		await publisher.close();
		await consumer.close();
	}
}

function gcMetrics(gc: ReturnType<typeof collections>) {
	return { gc_count: gc.count, gc_pause_ms: gc.pause_ms, gc_reclaimed_bytes: gc.reclaimed_bytes };
}

async function cmaf(variant: string, shape: Cmaf, round: number, pass: Pass) {
	const key = `cmaf ${shape.kind}`;
	const opened = await open(variant, "cmaf");
	try {
		await opened.page.evaluate((shape) => window.cmaf.prepare(shape), shape);
		const expected = shape.sizes.reduce((sum, size, frame) => {
			const pattern = shape.kind === "video" && frame > 0 ? 1 : 0;
			return (sum + check(payload(size, pattern), size, pattern, false)) >>> 0;
		}, 0);
		for (const phase of ["decode"] as const) {
			const before = await metrics(opened);
			let result: Phase | undefined;
			const run = async () => {
				result = await bounded(
					opened.page.evaluate((phase) => window.cmaf[phase](), phase),
					`${key} ${phase}`,
				);
			};
			const events = await trace(pass, opened.page, `cmaf-${shape.kind}-${phase}-${variant}`, run);
			const after = await metrics(opened);
			const checked = await opened.page.evaluate(() => window.cmaf.verify());
			if (opened.errors.length) throw new Error(opened.errors.join("; "));
			if (!result) throw new Error("no result");
			if (checked !== expected) throw new Error(`${key} ${phase} ${variant}: digest ${checked} != ${expected}`);
			const row: Row = {
				scenario: "cmaf",
				key: `${key} ${phase}`,
				shape: { kind: shape.kind, frames: shape.sizes.length, iterations: shape.iterations },
				variant,
				round,
				pass,
				delivered: { frames: result.fragments, bytes: result.bytes, digest: checked },
				metrics: {
					wall_ms: result.elapsed_ms,
					main_ms: (after.main_s - before.main_s) * 1000,
					per_fragment_us: (result.elapsed_ms * 1000) / result.fragments,
					long_task_ms: result.long_tasks.total_ms,
					...(events && gcMetrics(collections(events, opened.pid))),
				},
				heap: { before: before.heap, after: after.heap },
				load: loadavg(),
			};
			record(row);
			log(row);
		}
	} finally {
		await opened.close();
	}
}

async function media(variant: string, round: number, pass: Pass) {
	const config: Media = { url: relay, path: `media/${crypto.randomUUID()}`, ...MEDIA, probe: pass === "probed" };
	const key = `media ${MEDIA.width}x${MEDIA.height}@${MEDIA.fps}`;
	const publisher = await open(variant, "media");
	const watcher = await open(variant, "media");
	try {
		await bounded(
			publisher.page.evaluate((config) => window.media.publish(config), config),
			"media publisher setup",
		);
		const { first_frame_ms } = await bounded(
			watcher.page.evaluate((config) => window.media.watch(config), config),
			"first presented frame",
		);
		await Bun.sleep(MEDIA_WARMUP_MS);
		const snapshot = async () => ({
			at: await watcher.page.evaluate(() => window.media.now()),
			published: await publisher.page.evaluate(() => window.media.published()),
			watched: await watcher.page.evaluate(() => window.media.watched()),
			watcher: await metrics(watcher),
			publisher: await metrics(publisher),
			processes: await processes(),
		});
		const before = await snapshot();
		const events = await trace(pass, watcher.page, `media-${variant}`, () => Bun.sleep(MEDIA_WINDOW_MS));
		const after = await snapshot();
		const inside = await watcher.page.evaluate(
			([start, end]) => window.media.window(start, end),
			[before.at, after.at],
		);
		const errors = [...publisher.errors, ...watcher.errors];
		if (errors.length) throw new Error(errors.join("; "));
		const seconds = (after.at - before.at) / 1000;
		const delta = {
			published: after.published.frames - before.published.frames,
			received: after.watched.received - before.watched.received,
			submitted: after.watched.decoder.submitted - before.watched.decoder.submitted,
			decoded: after.watched.decoder.decoded - before.watched.decoder.decoded,
			presented: after.watched.presented - before.watched.presented,
		};
		// Invalid, not slow: a window that presented nothing measured nothing.
		if (delta.published <= 0 || delta.decoded <= 0 || delta.presented <= 0)
			throw new Error(
				`${key} ${variant}: published ${delta.published}, decoded ${delta.decoded}, presented ${delta.presented}`,
			);
		let delay: Record<string, number> | undefined;
		if (pass === "probed") {
			const paints = new Map(await publisher.page.evaluate(() => window.media.paints()));
			const presented = await watcher.page.evaluate(
				([start, end]) => window.media.presentations(start, end),
				[before.at, after.at],
			);
			const delays = presented
				.filter(([id]) => paints.has(id))
				.map(([id, at]) => at - (paints.get(id) ?? 0))
				.sort((a, b) => a - b);
			// Presented before it was painted: a misread id or a broken clock, never a fast frame.
			if (delays[0] < 0) throw new Error(`${key} ${variant}: negative frame delay ${delays[0]}ms`);
			if (delays.length === 0) throw new Error(`${key} ${variant}: no presented frame id could be read back`);
			const at = (q: number) => delays[Math.min(delays.length - 1, Math.floor(q * delays.length))];
			delay = {
				frame_delay_p50_ms: at(0.5),
				frame_delay_p95_ms: at(0.95),
				frame_delay_p99_ms: at(0.99),
				frame_delay_max_ms: delays[delays.length - 1],
				frame_ids_read: delays.length,
			};
		}
		const row: Row = {
			scenario: "media",
			key,
			shape: { ...MEDIA, codec_resolved: after.published.codec, window_s: seconds },
			variant,
			round,
			pass,
			delivered: {
				frames: delta.presented,
				bytes: after.watched.bytes - before.watched.bytes,
				digest: delta.published,
			},
			metrics: {
				first_frame_ms,
				published_fps: delta.published / seconds,
				received_fps: delta.received / seconds,
				decoded_fps: delta.decoded / seconds,
				presented_fps: delta.presented / seconds,
				decode_backlog: delta.submitted - delta.decoded,
				render_skipped: delta.decoded - delta.presented,
				decode_queue_mean:
					(after.watched.decoder.queue_sum - before.watched.decoder.queue_sum) / Math.max(1, delta.submitted),
				decode_queue_max: after.watched.decoder.queue_max,
				watcher_main_ms_per_s: ((after.watcher.main_s - before.watcher.main_s) * 1000) / seconds,
				publisher_main_ms_per_s: ((after.publisher.main_s - before.publisher.main_s) * 1000) / seconds,
				...Object.fromEntries(
					Object.entries(
						processCpu(before.processes, after.processes, {
							watcher: watcher.pid,
							publisher: publisher.pid,
						}),
					).map(([name, value]) => [`${name}_per_s`, value / seconds]),
				),
				long_task_ms_per_s: inside.long_tasks.total_ms / seconds,
				heap_slope_kib_per_s: slope(inside.heap) / 1024,
				...(events && gcMetrics(collections(events, watcher.pid))),
				...delay,
			},
			heap: { before: before.watched.heap, after: after.watched.heap },
			load: loadavg(),
		};
		record(row);
		log(row);
		// Page-side cleanup only after success; see transport.
		await watcher.page.evaluate(() => window.media.stop());
		await publisher.page.evaluate(() => window.media.stop());
	} finally {
		await watcher.close();
		await publisher.close();
	}
}

async function sampling(variant: string, shape: SamplingShape, round: number, pass: Pass) {
	const { absent, ...rest } = shape;
	const config: Sampling = { url: relay, path: `stats/${crypto.randomUUID()}`, ...rest };
	const key = `stats ${shape.mode}${absent ? " absent" : ""} c${shape.connections} r${shape.reservations}`;
	const init = [countCalls];
	if (shape.mode === "qmux") init.push(withoutWebTransport);
	if (absent) init.push(withoutGetStats);
	if (!exposed) throw new Error("the stats browser is not running");
	const opened = await open(variant, "stats", false, init, exposed);
	try {
		await bounded(
			opened.page.evaluate((config) => window.stats.setup(config), config),
			`${key} setup`,
		);
		await Bun.sleep(SAMPLING_SETTLE_MS);
		const span = shape.mode === "linger" ? LINGER_WINDOW_MS : SAMPLING_WINDOW_MS;
		const snapshot = async () => ({
			at: performance.now(),
			counts: await opened.page.evaluate(() => window.stats.counts()),
			outcome: await opened.page.evaluate(() => window.stats.outcome()),
			page: await metrics(opened),
			processes: await processes(exposed.system),
		});
		const before = await snapshot();
		if (pass === "counted") {
			// Collected objects included: the cost is what the window allocated, not what it kept.
			await opened.cdp.send("HeapProfiler.enable");
			await opened.cdp.send("HeapProfiler.startSampling", {
				samplingInterval: 256,
				includeObjectsCollectedByMajorGC: true,
				includeObjectsCollectedByMinorGC: true,
			});
		}
		const events = await trace(
			pass,
			opened.page,
			`${key.replaceAll(" ", "-")}-${variant}`,
			() => Bun.sleep(span),
			exposed.browser,
		);
		const allocated =
			pass === "counted" ? allocatedBytes((await opened.cdp.send("HeapProfiler.stopSampling")).profile.head) : 0;
		const after = await snapshot();
		if (opened.errors.length) throw new Error(opened.errors.join("; "));
		const seconds = (after.at - before.at) / 1000;
		verify(key, variant, shape, before.outcome, after.outcome);
		const rate = (value: number) => value / seconds;
		const cpu = processCpu(before.processes, after.processes, { renderer: opened.pid });
		const delta = (name: keyof Counts) => after.counts[name] - before.counts[name];
		const row: Row = {
			scenario: "stats",
			key,
			shape: { ...shape, transport: after.outcome.transport, window_s: seconds },
			variant,
			round,
			pass,
			delivered: {
				frames: after.outcome.received - before.outcome.received,
				bytes: 0,
				digest: after.outcome.estimated,
			},
			metrics: {
				getstats_per_s: rate(delta("getstats")),
				timers_per_s: rate(delta("timers")),
				main_ms_per_s: rate((after.page.main_s - before.page.main_s) * 1000),
				...Object.fromEntries(Object.entries(cpu).map(([name, value]) => [`${name}_per_s`, rate(value)])),
				estimated: after.outcome.estimated,
				received_fps: rate(after.outcome.received - before.outcome.received),
				...(pass === "counted" && { allocated_kib_per_s: rate(allocated / 1024) }),
				...(events && gcMetrics(collections(events, opened.pid))),
			},
			heap: { before: before.page.heap, after: after.page.heap },
			load: loadavg(),
		};
		record(row);
		log(row);
		await opened.page.evaluate(() => window.stats.stop());
	} finally {
		await opened.close();
	}
}

// The window must leave every connection working, so doing less cannot read as cheaper.
function verify(key: string, variant: string, shape: SamplingShape, before: Outcome, after: Outcome) {
	const fail = (what: string) => {
		throw new Error(`${key} ${variant}: ${what}`);
	};
	if (after.connected !== shape.connections) fail(`${after.connected} of ${shape.connections} connected`);
	const distinct = shape.mode === "pooled" ? 1 : shape.connections;
	if (after.estimated > distinct * shape.reservations) fail(`${after.estimated} estimates`);
	// Every playing connection reads frames throughout the window.
	if (shape.mode === "receive" && after.received - before.received < shape.connections)
		fail(`received ${after.received - before.received} frames in the window`);
}

// Bytes a sampled allocation profile attributes, summed over the whole call tree.
function allocatedBytes(node: { selfSize: number; children: { selfSize: number; children: unknown[] }[] }): number {
	let sum = node.selfSize;
	for (const child of node.children) sum += allocatedBytes(child as typeof node);
	return sum;
}

// Releases and re-takes a reservation on an idle connection, timing its first send-rate slice.
async function resume(variant: string, round: number) {
	const config = {
		url: relay,
		path: `stats/${crypto.randomUUID()}`,
		mode: "private" as const,
		connections: 1,
		reservations: 0,
		...RESUME,
	};
	const key = `stats resume x${RESUME.cycles}`;
	if (!exposed) throw new Error("the stats browser is not running");
	const opened = await open(variant, "stats", false, [countCalls], exposed);
	try {
		const waits = await bounded(
			opened.page.evaluate((config) => window.stats.resume(config), config),
			`${key} cycles`,
		);
		if (opened.errors.length) throw new Error(opened.errors.join("; "));
		const found = waits.filter(Number.isFinite).sort((a, b) => a - b);
		const at = (q: number) => found[Math.min(found.length - 1, Math.floor(q * found.length))] ?? Number.NaN;
		const row: Row = {
			scenario: "stats",
			key,
			shape: { ...RESUME },
			variant,
			round,
			pass: "timed",
			delivered: { frames: found.length, bytes: 0, digest: waits.length },
			metrics: {
				first_estimate_p50_ms: at(0.5),
				first_estimate_max_ms: found.at(-1) ?? Number.NaN,
				first_estimate_missing: waits.length - found.length,
			},
			heap: { before: null, after: null },
			load: loadavg(),
		};
		record(row);
		log(row);
		await opened.page.evaluate(() => window.stats.stop());
	} finally {
		await opened.close();
	}
}

// Least-squares bytes per second over [ms, bytes] samples; 0 with fewer than two.
function slope(points: [number, number][]): number {
	if (points.length < 2) return 0;
	const n = points.length;
	const mx = points.reduce((sum, [x]) => sum + x, 0) / n;
	const my = points.reduce((sum, [, y]) => sum + y, 0) / n;
	const num = points.reduce((sum, [x, y]) => sum + (x - mx) * (y - my), 0);
	const den = points.reduce((sum, [x]) => sum + (x - mx) ** 2, 0);
	return den === 0 ? 0 : (num / den) * 1000;
}

function log(row: Row) {
	const shown = Object.entries(row.metrics)
		.slice(0, 4)
		.map(([name, value]) => `${name}=${value.toFixed(2)}`)
		.join(" ");
	console.log(`${row.key} ${row.variant} ${row.pass}${row.round >= 0 ? ` r${row.round}` : ""}: ${shown}`);
}

async function printComparisons() {
	const lines: string[] = [];
	const line = (text: string) => {
		console.log(text);
		lines.push(text);
	};
	line("\nPaired change, variant against reference (median of per-round deltas [range]; host load varies):");
	for (const entry of report.comparisons as ReturnType<typeof compare>) {
		if (!HEADLINE.has(entry.metric)) continue;
		const sign = (value: number) => `${value >= 0 ? "+" : ""}${value.toFixed(1)}`;
		line(
			`  ${entry.key} ${entry.variant} vs ${entry.reference} ${entry.metric}: ${sign(entry.median_pct)}% [${sign(entry.min_pct)}, ${sign(entry.max_pct)}] n=${entry.pairs}${entry.consistent ? "" : " (sign varies)"}`,
		);
	}
	line("\nInstrumented passes (one sample each):");
	for (const row of report.samples) {
		if (row.pass === "timed") continue;
		const shown = Object.entries(row.metrics)
			.filter(([name]) => INSTRUMENTED.test(name))
			.map(([name, value]) => `${name}=${Number.isInteger(value) ? value : value.toFixed(3)}`);
		line(`  ${row.key} ${row.variant} ${row.pass}: ${shown.join(" ")}`);
	}
	if (process.env.GITHUB_STEP_SUMMARY) {
		await appendFile(
			process.env.GITHUB_STEP_SUMMARY,
			`## Browser benchmark\n\n\`\`\`text\n${lines.join("\n")}\n\`\`\`\n`,
		);
	}
}
