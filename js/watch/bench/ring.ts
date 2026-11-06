/**
 * Sweep players sharing one AudioContext against the main-thread cost of feeding the audio ring, in
 * headless Chromium: CPU from thread ticks, and sampled JS allocation in a separate window so the
 * profiler does not inflate the CPU. Each player inserts stereo 20 ms frames into the real
 * `AudioBuffer` and render worklet. Fails only when playback does not run, never on timing.
 */
import { basename } from "node:path";
import { type Browser, chromium } from "playwright";

const WARMUP_MS = 2_000;
const WINDOW_MS = 10_000;
const ROUNDS = 3;

const build = await Bun.build({
	entrypoints: [`${import.meta.dir}/ring-page.ts`, `${import.meta.dir}/../src/audio/render-worklet.ts`],
	target: "browser",
	minify: true,
});
if (!build.success) throw new AggregateError(build.logs, "ring benchmark failed to bundle");
const files = new Map(
	await Promise.all(build.outputs.map(async (file) => [`/${basename(file.path)}`, await file.text()] as const)),
);
const server = Bun.serve({
	port: 0,
	hostname: "127.0.0.1",
	fetch(request) {
		const script = files.get(new URL(request.url).pathname);
		if (script) return new Response(script, { headers: { "Content-Type": "text/javascript" } });
		return new Response('<script type="module" src="/ring-page.js"></script>', {
			headers: { "Content-Type": "text/html" },
		});
	},
});

interface Stats {
	callbacks: number;
	context: AudioContextState;
	players: { timestamp: number; stalled: boolean; underruns: number }[];
}
const stats = () => (globalThis as unknown as { ring: { stats(): Stats } }).ring.stats();
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const median = (values: number[]) => values.toSorted((a, b) => a - b)[Math.floor(values.length / 2)];

let browser: Browser | undefined;
try {
	browser = await chromium.launch({
		channel: "chromium",
		headless: true,
		args: [
			"--autoplay-policy=no-user-gesture-required",
			"--disable-background-timer-throttling",
			"--disable-renderer-backgrounding",
			"--disable-backgrounding-occluded-windows",
		],
	});
	console.log("players,cpu_ms_per_s,cpu_us_per_callback,js_kib_per_s,js_bytes_per_callback,underruns");
	for (const players of [1, 4, 16]) {
		const rounds: { cpuMs: number; cpuUs: number; kib: number; bytes: number }[] = [];
		let underruns = 0;
		for (let round = 0; round < ROUNDS; round++) {
			const page = await browser.newPage();
			const errors: string[] = [];
			page.on("pageerror", (error) => errors.push(String(error)));
			const cdp = await page.context().newCDPSession(page);
			await cdp.send("Performance.enable", { timeDomain: "threadTicks" });
			await page.goto(`http://127.0.0.1:${server.port}/?players=${players}`);
			await page.waitForFunction(() => "ring" in globalThis);
			await sleep(WARMUP_MS);

			const metric = (metrics: { name: string; value: number }[], name: string) => {
				const found = metrics.find((m) => m.name === name);
				if (!found) throw new Error(`missing metric ${name}`);
				return found.value;
			};
			// One window: elapsed seconds, main-thread task seconds, and callbacks across every player.
			const measure = async () => {
				const before = (await cdp.send("Performance.getMetrics")).metrics;
				const start = await page.evaluate(stats);
				await sleep(WINDOW_MS);
				const end = await page.evaluate(stats);
				const after = (await cdp.send("Performance.getMetrics")).metrics;
				return {
					seconds: metric(after, "Timestamp") - metric(before, "Timestamp"),
					task: metric(after, "TaskDuration") - metric(before, "TaskDuration"),
					callbacks: (end.callbacks - start.callbacks) * players,
					start,
					end,
				};
			};

			const first = await measure();
			await cdp.send("HeapProfiler.startSampling", {
				samplingInterval: 1024,
				includeObjectsCollectedByMajorGC: true,
				includeObjectsCollectedByMinorGC: true,
			});
			const second = await measure();
			const { profile } = await cdp.send("HeapProfiler.stopSampling");
			const bytes = profile.samples.reduce((sum, sample) => sum + sample.size, 0);
			const result = {
				cpuMs: (first.task * 1000) / first.seconds,
				cpuUs: (first.task * 1e6) / first.callbacks,
				kib: bytes / 1024 / second.seconds,
				bytes: bytes / second.callbacks,
			};
			rounds.push(result);

			const { start } = first;
			const { end } = second;
			if (errors.length > 0) throw new Error(`page errors: ${errors.join("; ")}`);
			if (end.context !== "running") throw new Error(`the AudioContext is ${end.context}`);
			for (const [i, player] of end.players.entries()) {
				// Two windows of wall time should play most of their audio.
				const played = player.timestamp - start.players[i].timestamp;
				if (played < WINDOW_MS * 1000)
					throw new Error(`player ${i} played ${played} us in ${2 * WINDOW_MS} ms`);
				underruns += player.underruns - start.players[i].underruns;
			}
			console.error(`players ${players} round ${round}: ${JSON.stringify(result)}`);

			await page.evaluate(() => (globalThis as unknown as { ring: { stop(): Promise<void> } }).ring.stop());
			await page.close();
		}
		const of = (key: keyof (typeof rounds)[number]) => median(rounds.map((round) => round[key]));
		console.log(
			`${players},${of("cpuMs").toFixed(3)},${of("cpuUs").toFixed(3)},${of("kib").toFixed(3)},${of("bytes").toFixed(1)},${underruns}`,
		);
	}
} finally {
	server.stop(true);
	await browser?.close();
}
