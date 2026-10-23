# Audio ring A/B, 2026-10-23

This is the pre-removal checkpoint for [the one-ring quest](/quest/m1/watch-audio-one-ring.md).
Both production rings and their selection remain unchanged. This records a one-off comparison,
not a new nightly benchmark or an acceptance threshold.

## Method

- Source: `fc6f2dde5a0f019dd3b0a4a9dde65d23b995ca95`, plus the empty quest claim.
- Browser: pinned headless Chromium 156.0.8078.4; host x86\_64, 24 logical CPUs.
- One page, one running 48 kHz AudioContext, and 1, 4 or 16 real render AudioWorklets.
  Each uses the unchanged `createAudioBuffer` with stereo PCM, 960 frames per callback,
  a 200 ms latency floor and unbuffered playback. Isolation selects the shared ring;
  the non-isolated document selects postMessage. Audio reaches the real destination.
- Fresh PCM channel arrays are produced on main in both paths. The shared writer copies them;
  postMessage transfers them. The source catches up packets due on the AudioContext clock,
  so relative timer drift cannot progressively underfeed the device.
- Every window gets 2 seconds of warmup and 10 seconds of measurement. Three rounds alternate
  shared/post order, with a second identical shared run (A/A) in each round.
- CPU: CDP `Performance` with `timeDomain: threadTicks`, difference of `TaskDuration`.
  Allocation: separate CDP HeapProfiler passes, 1024-byte sampling, including objects collected
  by minor and major GC. These are sampled JS allocation estimates, not retained-heap deltas.
- All 54 corrected windows completed: every playhead advanced, no page errors, correct isolation,
  running contexts, no final stalls and zero cumulative underruns. Delivery counts are retained below.

## Measurements

Medians of three windows. CPU comes only from the unprofiled CPU pass. Allocation comes only
from the separate sampled pass. A callback here is one player inserting 20 ms of stereo PCM.

| Players | Ring | Main CPU ms/s | CPU us/callback | JS allocation KiB/s | JS bytes/callback |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | shared | 5.902 | 118.359 | 43.297 | 885.5 |
| 1 | shared-aa | 6.065 | 121.188 | 45.032 | 925.0 |
| 1 | post | 7.628 | 153.006 | 51.545 | 1054.0 |
| 4 | shared | 6.889 | 34.552 | 147.832 | 755.8 |
| 4 | shared-aa | 7.273 | 36.461 | 144.751 | 743.1 |
| 4 | post | 10.513 | 52.502 | 187.512 | 962.4 |
| 16 | shared | 10.976 | 13.703 | 544.690 | 697.3 |
| 16 | shared-aa | 11.660 | 14.553 | 554.834 | 712.2 |
| 16 | post | 20.913 | 26.105 | 687.705 | 883.2 |

Paired per-round CPU deltas, in ms/s. Ranges describe these observations, not confidence intervals.

| Players | Post minus shared median (range) | A/A minus shared median (range) |
| --- | ---: | ---: |
| 1 | 1.026 (0.847 to 2.374) | -0.009 (-0.716 to 0.775) |
| 4 | 3.675 (1.306 to 4.221) | 0.575 (-2.424 to 1.927) |
| 16 | 9.638 (9.030 to 11.185) | 0.684 (0.212 to 1.250) |

## Interpretation and limits

PostMessage costs more in this ring-only main-thread fixture, including after normalization
by delivered callbacks. The absolute CPU and allocation increments are the tradeoff to review.
A relative increase in ring cost is not a relative increase in total player cost, and no numeric
meaning of “materially more” has been inferred.

This excludes network, codec decode, full Decoder/Sync effects, UI work and their allocations.
The shared AudioContext isolates ring overhead; it does not model one complete player graph
per independent context. Worklet-thread CPU and allocations are outside the main-thread metrics.
Native ArrayBuffer backing stores, IPC/serialization allocations and process memory are unmeasured.
Both paths explicitly generate 7680 PCM payload bytes per callback before transfer or copy;
that known input volume is not a measurement of native allocation churn.
The A/A control exposes local timing spread; host load was not isolated or sampled per window.

The original fixture used consecutive relative 20 ms timers. That slowly supplied fewer than
50 callbacks/s and eventually ended a postMessage allocation window stalled (exit 1 after
35 retained windows). Those measurements are withdrawn. The corrected fixture below preserves
the playback assertion and fixes pacing at its source. Original logs remain local evidence,
not a passing comparison.

Recommendation: accept this small absolute increase and finish the approved simplification,
given the existing listener-quality evidence and uncommon production isolation. Keep this
checkpoint draft until the maintainer judges that tradeoff: the fixture does not establish
the relative cost of a full player. The alternative is to retain both paths and revise the
removal timing. The original one-ring Goal remains unmet; this report does not replace it
with a benchmark-only Goal or declare the undefined materiality condition failed.

## Raw corrected windows

Frames are source callback ticks per player; total buffer insert calls are `frames * players`.
CPU in profiled allocation windows is retained for diagnostics and is excluded from the CPU comparison.

```csv
pass,players,round,ring,seconds,frames_per_player,cpu_ms,estimated_js_allocation_bytes
cpu,1,0,shared,10.006854,499,59.061,
cpu,1,0,shared-aa,10.004537,501,58.959,
cpu,1,0,post,10.004363,499,69.314,
cpu,1,1,post,10.009003,499,76.35,
cpu,1,1,shared,10.007219,501,67.861,
cpu,1,1,shared-aa,10.010642,501,60.715,
cpu,1,2,shared,10.0059,501,56.295,
cpu,1,2,shared-aa,10.005049,499,64.044,
cpu,1,2,post,10.006269,501,80.047,
cpu,4,0,shared,10.010369,499,68.965,
cpu,4,0,shared-aa,10.016961,499,88.31,
cpu,4,0,post,10.046192,501,111.614,
cpu,4,1,post,10.007647,501,105.214,
cpu,4,1,shared,10.008817,499,92.154,
cpu,4,1,shared-aa,10.0055,501,67.869,
cpu,4,2,shared,10.004849,499,67.012,
cpu,4,2,shared-aa,10.006478,499,72.776,
cpu,4,2,post,10.005997,501,103.794,
cpu,16,0,shared,10.005888,501,118.908,
cpu,16,0,shared-aa,10.009212,501,131.458,
cpu,16,0,post,10.006008,501,209.26,
cpu,16,1,post,10.018251,499,222.013,
cpu,16,1,shared,10.007637,501,109.847,
cpu,16,1,shared-aa,10.004948,501,116.66,
cpu,16,2,shared,10.00586,499,101.443,
cpu,16,2,shared-aa,10.004959,501,103.555,
cpu,16,2,post,10.005641,499,197.878,
alloc,1,0,shared,10.005849,501,66.66,443616
alloc,1,0,shared-aa,10.010203,499,70.758,461596
alloc,1,0,post,10.0049,501,81.802,528076
alloc,1,1,post,10.005426,499,82.663,496692
alloc,1,1,shared,10.003452,495,61.966,432980
alloc,1,1,shared-aa,10.003509,501,51.295,466628
alloc,1,2,shared,10.004844,501,52.14,458236
alloc,1,2,shared-aa,10.006778,499,63.039,443736
alloc,1,2,post,10.004563,499,79.761,550592
alloc,4,0,shared,10.022978,501,80.665,1524564
alloc,4,0,shared-aa,10.015388,499,89.487,1396428
alloc,4,0,post,10.004868,499,101.091,1847668
alloc,4,1,post,10.004215,499,95.18,1920932
alloc,4,1,shared,10.004718,501,80.282,1465628
alloc,4,1,shared-aa,10.005748,501,80.888,1529668
alloc,4,2,shared,10.00548,501,79.066,1514632
alloc,4,2,shared-aa,10.006022,499,104.551,1483140
alloc,4,2,post,10.015317,501,113.124,2031344
alloc,16,0,shared,10.008245,499,155.548,5482144
alloc,16,0,shared-aa,10.004413,499,136.416,5740836
alloc,16,0,post,10.00467,499,228.132,6962728
alloc,16,1,post,10.01288,499,240.845,7051168
alloc,16,1,shared,10.020987,501,162.044,5589332
alloc,16,1,shared-aa,10.008362,499,163.061,5686252
alloc,16,2,shared,10.006592,501,162.257,5676168
alloc,16,2,shared-aa,10.006561,501,134.281,5650176
alloc,16,2,post,10.005294,501,197.505,7248200
```

## Reproduce the one-off comparison

After installing pinned workspace dependencies, save these two source snapshots as
`.scratch/ring-ab/page.ts` and `.scratch/ring-ab/run.ts` in this checkout. All generated
bundles, profiles and results stay under that ignored worktree directory.

```bash
nix develop --command bun install --frozen-lockfile
nix develop --command bun .scratch/ring-ab/run.ts
```

### page.ts

```typescript
import { createAudioBuffer } from "../../js/watch/src/audio/buffer";
import { Micro } from "@moq/clock";

const context = new AudioContext({ sampleRate: 48000, latencyHint: "interactive" });
await context.audioWorklet.addModule("/render-worklet.js");
const players = [];
const count = Number(new URL(location.href).searchParams.get("players"));
for (let i = 0; i < count; i++) {
	const worklet = new AudioWorkletNode(context, "render", { outputChannelCount: [2] });
	worklet.connect(context.destination);
	const buffer = createAudioBuffer(worklet, 2, 48000, 9600);
	players.push({ worklet, buffer });
}
await context.resume();
let frames = 0;
let running = true;
const start = context.currentTime;
const template = new Float32Array(960);
for (let i = 0; i < template.length; i++) template[i] = Math.sin((i / 48000) * 440 * 2 * Math.PI) * 0.01;
const tick = () => {
	if (!running) return;
	// Pace from the device clock: relative timers alone slowly underfeed a real worklet.
	while (frames * 0.02 <= context.currentTime - start) {
		for (const { buffer } of players) buffer.insert(Micro(frames * 20000), [template.slice(), template.slice()]);
		frames++;
	}
	setTimeout(tick, 20);
};
tick();
Object.assign(globalThis, {
	stats: () => ({ frames, players: players.map(({ buffer }) => ({ timestamp: buffer.timestamp.peek(), underruns: buffer.underruns.peek(), stalled: buffer.stalled.peek() })), context: context.state, isolated: crossOriginIsolated }),
	stop: async () => { running = false; for (const { buffer, worklet } of players) { buffer.close(); worklet.disconnect(); } await context.close(); },
});
```

### run.ts

```typescript
import { chromium } from "playwright";
import { join } from "node:path";
const root = import.meta.dirname;
for (const [entry, outfile] of [[join(root, "page.ts"), "page.js"], [join(root, "../../js/watch/src/audio/render-worklet.ts"), "render-worklet.js"]]) {
	const result = await Bun.build({ entrypoints: [entry], target: "browser", minify: true });
	if (!result.success) throw new Error(String(result.logs));
	await Bun.write(join(root, outfile), result.outputs[0]);
}
const server = Bun.serve({ port: 0, hostname: "127.0.0.1", async fetch(req) {
	const url = new URL(req.url);
	const isolated = url.pathname.startsWith("/shared/");
	const headers = isolated ? { "Cross-Origin-Opener-Policy": "same-origin", "Cross-Origin-Embedder-Policy": "require-corp" } : {};
	if (url.pathname === "/favicon.ico") return new Response(null, { status: 204 });
	if (url.pathname.endsWith("/")) return new Response('<script type="module" src="/page.js"></script>', { headers: { ...headers, "Content-Type": "text/html" } });
	return new Response(Bun.file(join(root, url.pathname)), { headers });
} });
const flags = ["--autoplay-policy=no-user-gesture-required", "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows"];
const browser = await chromium.launch({ channel: "chromium", headless: true, args: flags });
const results = [];
const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));
try {
	for (const pass of ["cpu", "alloc"]) for (const players of [1, 4, 16]) for (let round = 0; round < 3; round++) {
		for (const ring of round % 2 ? ["post", "shared", "shared-aa"] : ["shared", "shared-aa", "post"]) {
			const page = await browser.newPage();
			const errors = [];
			page.on("pageerror", error => errors.push(String(error)));
			const cdp = await page.context().newCDPSession(page);
			await cdp.send("Performance.enable", { timeDomain: "threadTicks" });
			await page.goto(`http://127.0.0.1:${server.port}/${ring === "post" ? "post" : "shared"}/?players=${players}`);
			await page.waitForFunction(() => typeof globalThis.stats === "function");
			await sleep(2000);
			if (pass === "alloc") await cdp.send("HeapProfiler.startSampling", { samplingInterval: 1024, includeObjectsCollectedByMajorGC: true, includeObjectsCollectedByMinorGC: true });
			const before = await cdp.send("Performance.getMetrics");
			const start = await page.evaluate(() => globalThis.stats());
			await sleep(10000);
			const end = await page.evaluate(() => globalThis.stats());
			const after = await cdp.send("Performance.getMetrics");
			let allocationBytes;
			if (pass === "alloc") {
				const { profile } = await cdp.send("HeapProfiler.stopSampling");
				allocationBytes = profile.samples.reduce((sum, sample) => sum + sample.size, 0);
				await Bun.write(join(root, `${pass}-${players}-${round}-${ring}.heap.json`), JSON.stringify(profile));
			}
			if (errors.length || end.context !== "running" || end.isolated !== (ring !== "post") || end.players.some(p => p.timestamp <= 0 || p.stalled)) throw new Error(JSON.stringify({ errors, end }));
			const metric = (r, name) => r.metrics.find(m => m.name === name).value;
			const seconds = metric(after, "Timestamp") - metric(before, "Timestamp");
			const result = { pass, players, round, ring, seconds, frames: end.frames - start.frames, cpuMs: (metric(after, "TaskDuration") - metric(before, "TaskDuration")) * 1000, allocationBytes, start, end };
			results.push(result);
			console.log(JSON.stringify(result));
			await Bun.write(join(root, "results.json"), JSON.stringify({ browser: browser.version(), flags, results }, null, 2));
			await page.evaluate(() => globalThis.stop());
			await page.close();
		}
	}
} finally { await browser.close(); server.stop(true); }
```
