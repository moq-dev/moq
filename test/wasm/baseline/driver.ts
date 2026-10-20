/** Retain raw backend samples; correctness fails, timing variability is reported. */
import { createHash } from "node:crypto";
import { readdir } from "node:fs/promises";
import { cpus, freemem, loadavg, platform, totalmem } from "node:os";
import { dirname, join } from "node:path";
import { parseArgs } from "node:util";
import { chromium } from "playwright";
import type { RelayFixture } from "../src/main.ts";
import type { Sample, Workload } from "./fixture.ts";

const { values } = parseArgs({ options: { relays: { type: "string" }, timeout: { type: "string", default: "30" } } });
const timeout = Number(values.timeout) * 1000;
const run = process.env.MOQ_TEST_RUN;
if (!values.relays || !run || !Number.isFinite(timeout) || timeout <= 0)
	throw new Error("baseline needs harness relays, retained run directory and positive timeout");
const here = dirname(new URL(import.meta.url).pathname);
const root = join(here, "../../..");
const dist = join(root, "test/wasm/dist");
const wasm = join(root, "js/wasm/dist");
const output = join(run, "backend.json");
const smoke = process.env.MOQ_WASM_BASELINE_SMOKE === "1";
// moq-ffi over ubrn's wasm2 player, built by uniffi/build.sh; see README.md.
const uniffi = process.env.MOQ_WASM_UNIFFI === "1";
const generated = join(root, "test/wasm/uniffi/dist");
const relays = (await Bun.file(values.relays).json()) as RelayFixture[];
if (!relays.length) throw new Error("no relay fixtures");
const server = Bun.serve({
	hostname: "127.0.0.1",
	port: 0,
	async fetch(req) {
		const path = new URL(req.url).pathname;
		if (path === "/publisher" || path === "/consumer")
			return new Response(`<script type="module" src="/${path.slice(1)}.js"></script>`, {
				headers: { "content-type": "text/html", "cache-control": "no-store" },
			});
		const file = path.startsWith("/wasm/")
			? Bun.file(join(wasm, path.slice(6)))
			: path.startsWith("/uniffi/")
				? Bun.file(join(generated, path.slice(8)))
				: Bun.file(join(dist, path));
		return (await file.exists())
			? new Response(file, { headers: { "cache-control": "no-store" } })
			: new Response("not found", { status: 404 });
	},
});
const endpoint = `http://127.0.0.1:${server.port}`;
const command = (...args: string[]) => {
	const result = Bun.spawnSync(args, { cwd: root });
	return result.exitCode === 0 ? result.stdout.toString().trim() : `unavailable: ${result.stderr.toString().trim()}`;
};
const asset = async (path: string) => {
	const bytes = new Uint8Array(await Bun.file(path).arrayBuffer());
	return {
		file: path.slice(root.length + 1),
		bytes: bytes.length,
		gzip: Bun.gzipSync(bytes).length,
		sha256: createHash("sha256").update(bytes).digest("hex"),
	};
};
async function wasmAssets(directory: string): Promise<string[]> {
	const entries = await readdir(directory, { withFileTypes: true });
	const files = await Promise.all(
		entries.map(async (entry) => {
			const path = join(directory, entry.name);
			return entry.isDirectory() ? wasmAssets(path) : /\.(js|wasm)$/.test(path) ? [path] : [];
		}),
	);
	return files.flat();
}
const summaryRows: { key: string; module: number; first: number; cpu: number; elapsed: number }[] = [];
const report = {
	version: 1,
	smoke,
	source: {
		revision: command("git", "rev-parse", "HEAD"),
		status: command("git", "status", "--porcelain"),
		features: "moq-wasm defaults; wasm-release; repository browser cfgs; bindgen web target; Binaryen not invoked",
		uniffi: uniffi
			? "moq-ffi defaults pinned to UniFFI 0.31 (uniffi/pin.patch); wasm-release; ubrn 0.31.0-6 wasm2; bundled with bun --minify"
			: "not run",
	},
	tools: {
		rust: command("rustc", "--version"),
		bindgen: command("wasm-bindgen", "--version"),
		binaryen: "not used by the current wasm build",
		bun: Bun.version,
		browser: "",
	},
	host: {
		platform: platform(),
		cpu: cpus()[0]?.model,
		logical_cpus: cpus().length,
		memory: { total: totalmem(), free: freemem() },
		load: loadavg(),
	},
	assets: {
		js: [await asset(join(dist, "net.js"))],
		wasm: await Promise.all((await wasmAssets(wasm)).map(asset)),
		// The bundled bindings, @ubjs runtime and wasm-bindgen glue, plus the staged module.
		uniffi: uniffi ? [await asset(join(dist, "uniffi.js")), await asset(join(generated, "moq_ffi.wasm"))] : [],
		harness: [await asset(join(dist, "consumer.js")), await asset(join(dist, "publisher.js"))],
	},
	limits: [
		"Complete entrypoint assets, uncompressed and gzip; neutral harness assets listed separately. No tree-shaken consumer-size claim.",
		"Fresh browser contexts and no-store assets. Browser process/OS/engine caches remain warm; module_ms measures an uncached page module import and initialization.",
		"CPU is consumer renderer TaskDuration in CDP threadTicks, including connect, protocol, boundary copies and byte validation. Those components cannot be individually attributed by current bindings.",
		"elapsed_ms includes session setup, relay and network waiting. first_frame_ms starts before backend module import. Publisher runs in another page, excluded from consumer renderer CPU.",
		"Heap snapshots and WASM buffer capacity are not peak/live allocation counts. GC and copy counts are unavailable, no inferred zero.",
		"Current WASM bindings consume only, have no WebSocket fallback and expose payloads without frame timestamps; JS public group sequences cannot represent values above 2^53 exactly.",
		"The existing WASM suite covers cancellation, refusal, close/error propagation, and chunked reads; forced memory growth and exact safe boundary sequences also run here. Complete publishing/backpressure/error-code parity remains an adoption gate.",
		"High u64 wire-codec fixtures run separately. A full high-u64 network/binding parity gate needs a native publisher fixture and expanded JS public group representation before adoption.",
	],
	samples: [] as unknown[],
	summary: [] as unknown[],
};
await Bun.write(output, JSON.stringify(report, null, 2));
const browser = await chromium.launch({ channel: "chromium", headless: true });
report.tools.browser = browser.version();
// Alternating and consecutive A/A rounds, so drift on a shared host hits every backend:
// three rounds each, in an order that puts every backend first, last, and twice in a row.
const backendsEnabled: Sample["backend"][] = ["js", "wasm", ...(uniffi ? (["uniffi"] as const) : [])];
const order: Sample["backend"][] =
	backendsEnabled.length === 2
		? ["js", "js", "wasm", "wasm", "js", "wasm"]
		: [...backendsEnabled, ...[...backendsEnabled].reverse(), ...backendsEnabled];
let serial = 0;
try {
	for (const relay of smoke ? relays.slice(0, 1) : relays) {
		const shapes = [
			...[160, 4096, 131072].flatMap((size) =>
				[1, 4].flatMap((publishers) =>
					[1, 4].map((subscribers) => ({
						size,
						publishers,
						subscribers,
						frames: 32,
						sequence: 0,
						grow: false,
					})),
				),
			),
			...[2 ** 31 - 1, 2 ** 32 - 1, Number.MAX_SAFE_INTEGER - 10000].map((sequence) => ({
				size: 160,
				publishers: 1,
				subscribers: 1,
				frames: 4,
				sequence,
				grow: true,
			})),
		];
		for (const shape of smoke ? [shapes[0], shapes[13]] : shapes) {
			const backends: Sample["backend"][] = smoke
				? ["js", "wasm", ...(uniffi ? (["uniffi"] as const) : [])]
				: order;
			for (const [round, backend] of backends.entries()) {
				const config: Sample = {
					...shape,
					id: `baseline/${serial++}`,
					url: relay.url,
					version: relay.version,
					backend,
					transport: "webtransport",
					module: `${endpoint}/${{ js: "net.js", wasm: "wasm/moq.js", uniffi: "uniffi.js" }[backend]}`,
				};
				await sample(config, round);
			}
		}
		// Exercise the shipped JS fallback on every protocol, without disguising the WASM gap.
		for (const [round, size] of [160, 131072].entries()) {
			await sample(
				{
					id: `baseline/${serial++}`,
					url: relay.url,
					version: relay.version,
					publishers: 1,
					subscribers: 1,
					frames: 32,
					size,
					sequence: 0,
					grow: false,
					backend: "js",
					transport: "websocket",
					module: `${endpoint}/net.js`,
				},
				round,
			);
		}
	}
} finally {
	await browser.close();
	server.stop(true);
	report.summary = summaries();
	await Bun.write(output, JSON.stringify(report, null, 2));
}
console.log(`Retained ${report.samples.length} raw backend samples: ${output}`);

async function sample(config: Sample, round: number) {
	const publisherContext = await browser.newContext();
	const consumerContext = await browser.newContext();
	const publisher = await publisherContext.newPage();
	const consumer = await consumerContext.newPage();
	const errors: string[] = [];
	for (const page of [publisher, consumer]) {
		page.on("pageerror", (error) => errors.push(error.message));
		// A panic in a spawned WASM task only reaches the console, never a pageerror or rejection.
		page.on("console", (message) => {
			if (message.text().includes("panicked at")) errors.push(`panic: ${message.text()}`);
		});
	}
	if (config.transport === "websocket")
		await consumer.addInitScript(() => {
			Object.defineProperty(globalThis, "WebTransport", { value: undefined });
		});
	const metrics = await consumer.context().newCDPSession(consumer);
	// Refuse an unavailable CPU clock rather than relabeling wall-task time as CPU.
	await metrics.send("Performance.enable", { timeDomain: "threadTicks" });
	const readMetrics = async () =>
		Object.fromEntries(
			(await metrics.send("Performance.getMetrics")).metrics.map((metric) => [metric.name, metric.value]),
		);
	try {
		await publisher.goto(`${endpoint}/publisher`);
		await publisher.evaluate((workload: Workload) => window.baselinePublish(workload), config);
		const before = await readMetrics();
		await consumer.goto(`${endpoint}/consumer`);
		const result = await bounded(consumer.evaluate((value) => window.baselineConsume(value), config));
		const after = await readMetrics();
		if (errors.length) throw new Error(errors.join("; "));
		if (
			!Number.isFinite(before.TaskDuration) ||
			!Number.isFinite(after.TaskDuration) ||
			after.TaskDuration < before.TaskDuration
		)
			throw new Error("invalid consumer CPU clock");
		const cpu = (after.TaskDuration - before.TaskDuration) * 1000;
		const row = {
			workload: {
				protocol: config.version,
				publishers: config.publishers,
				subscribers: config.subscribers,
				size: config.size,
				frames: config.frames,
				sequence: String(config.sequence),
			},
			round,
			...result,
			cpu: { milliseconds: cpu, per_frame_ms: cpu / result.frames.count },
			heap: { before: before.JSHeapUsedSize ?? null, after: after.JSHeapUsedSize ?? null },
			load: loadavg(),
		};
		report.samples.push(row);
		if (result.startup.first_frame_ms === undefined) throw new Error("sample has no first frame");
		summaryRows.push({
			key: JSON.stringify({ ...row.workload, backend: config.backend, transport: config.transport }),
			module: result.startup.module_ms,
			first: result.startup.first_frame_ms,
			cpu: row.cpu.per_frame_ms,
			elapsed: result.frames.elapsed_ms,
		});
		await Bun.write(output, JSON.stringify(report, null, 2));
		console.log(
			`${config.version} ${config.backend}/${config.transport} ${config.publishers}x${config.subscribers} ${config.size}B round${round}: ${cpu.toFixed(3)}ms consumer CPU`,
		);
	} finally {
		await publisher.evaluate(() => window.baselineStop()).catch(() => {});
		await publisherContext.close();
		await consumerContext.close();
	}
}

async function bounded<T>(task: Promise<T>): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			task,
			new Promise<never>((_resolve, reject) => {
				timer = setTimeout(() => reject(new Error("backend sample did not finish")), timeout);
			}),
		]);
	} finally {
		clearTimeout(timer);
	}
}

function summaries() {
	const groups = new Map<string, typeof summaryRows>();
	for (const row of summaryRows) {
		const group = groups.get(row.key) ?? [];
		group.push(row);
		groups.set(row.key, group);
	}
	const range = (values: number[]) => {
		values.sort((a, b) => a - b);
		const middle = Math.floor(values.length / 2);
		return {
			min: values[0],
			median: values.length % 2 ? values[middle] : (values[middle - 1] + values[middle]) / 2,
			max: values[values.length - 1],
		};
	};
	return [...groups].map(([key, rows]) => ({
		workload: JSON.parse(key),
		rounds: rows.length,
		startup: {
			module_ms: range(rows.map((row) => row.module)),
			first_frame_ms: range(rows.map((row) => row.first)),
		},
		cpu: { per_frame_ms: range(rows.map((row) => row.cpu)) },
		frames: { elapsed_ms: range(rows.map((row) => row.elapsed)) },
	}));
}
