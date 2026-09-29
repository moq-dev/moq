/**
 * Records one arrival trace in headless Chromium, and writes it as a checked-in `Trace`.
 *
 * The recorder page subscribes to the broadcast's audio and stamps every frame the container consumer
 * hands over. With `--mic`, a second page first publishes Chromium's fake capture device through
 * `<moq-publish>` to the same relay and broadcast, so the trace is a browser publisher's cadence.
 *
 *     bun record.ts --page dist --url https://cdn.moq.dev/demo --broadcast bbb.hang --duration 35 \
 *         --source "..." --description "..." --out ../../traces/relay-bbb.json
 *
 * The recording starts at the first arrival, tune-in burst included, since a player tunes in too.
 *
 * @module
 */
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import type { Page } from "playwright";
import { launch, sleep } from "../../../interop/clients/js/harness.ts";
import type { Arrival, Trace } from "./src/schema.ts";

const { values } = parseArgs({
	options: {
		page: { type: "string" },
		url: { type: "string" },
		broadcast: { type: "string" },
		duration: { type: "string", default: "35" },
		source: { type: "string" },
		description: { type: "string" },
		mic: { type: "boolean", default: false },
		out: { type: "string" },
	},
});

const { page: pageDir, url, broadcast, source, description, out } = values;
const durationMs = Number(values.duration) * 1000;
if (!pageDir || !url || !broadcast || !source || !description || !out || !Number.isFinite(durationMs) || durationMs <= 0) {
	console.error(
		"usage: record.ts --page DIR --url U --broadcast B --source S --description D --out FILE [--duration S] [--mic]",
	);
	process.exit(2);
}

/** How long the session and the first arrival get before the recording is abandoned. */
const STARTUP_MS = 30_000;

// Cross-origin isolated, for the 5 us `performance.now()` a non-isolated page coarsens to 100 us.
const root = resolve(pageDir);
const server = Bun.serve({
	port: 0,
	hostname: "127.0.0.1",
	async fetch(req) {
		const file = Bun.file(join(root, new URL(req.url).pathname));
		if (!(await file.exists())) return new Response("not found", { status: 404 });
		return new Response(file, {
			headers: { "cross-origin-opener-policy": "same-origin", "cross-origin-embedder-policy": "require-corp" },
		});
	},
});
const pageUrl = (file: string) => `http://127.0.0.1:${server.port}/${file}?${new URLSearchParams({ url, broadcast })}`;

/** Poll the page until `ready` returns a value, or throw naming what never happened. */
async function waitFor<T>(page: Page, what: string, ready: () => T | undefined): Promise<T> {
	const deadline = Date.now() + STARTUP_MS;
	while (Date.now() < deadline) {
		const error = await page.evaluate(() => globalThis.recorder?.error());
		if (error) throw new Error(`recorder: ${error}`);
		const value = await page.evaluate(ready);
		if (value !== undefined && value !== null) return value;
		await sleep(200);
	}
	throw new Error(`${what} not within ${STARTUP_MS / 1000}s`);
}

const browser = await launch(["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"]);
let failed = false;
try {
	const context = await browser.newContext({ permissions: ["microphone", "camera"] });
	if (values.mic) {
		const publisher = await context.newPage();
		publisher.on("pageerror", (error) => console.error(`[mic error] ${error.message}`));
		await publisher.goto(pageUrl("mic.html"), { waitUntil: "load" });
		// Subscribing before the element has an audio rendition finds no catalog to read.
		const deadline = Date.now() + STARTUP_MS;
		while (!(await publisher.evaluate(() => document.querySelector("moq-publish")?.audio.out.catalog.peek()))) {
			if (Date.now() > deadline) throw new Error(`the microphone never published within ${STARTUP_MS / 1000}s`);
			await sleep(200);
		}
		await sleep(2000);
	}

	const page = await context.newPage();
	page.on("pageerror", (error) => console.error(`[recorder error] ${error.message}`));
	page.on("console", (msg) => {
		if (msg.type() === "error" || msg.type() === "warning") console.error(`[recorder ${msg.type()}] ${msg.text()}`);
	});
	await page.goto(pageUrl("recorder.html"), { waitUntil: "load" });

	const info = await waitFor(page, "an audio subscription", () => globalThis.recorder.info());
	const arrivals: Arrival[] = [];
	await waitFor(page, "a first arrival", () => {
		const drained = globalThis.recorder.drain();
		return drained.length > 0 ? drained : undefined;
	}).then((first) => arrivals.push(...first));

	// Ends on the page's clock, not on the next arrival, so a publisher that goes quiet is recorded
	// as the silence it is instead of holding the recording open.
	const start = arrivals[0][0];
	while ((await page.evaluate(() => performance.now())) < start + durationMs) {
		await sleep(1000);
		const error = await page.evaluate(() => globalThis.recorder.error());
		if (error) throw new Error(`recorder: ${error}`);
		arrivals.push(...(await page.evaluate(() => globalThis.recorder.drain())));
	}
	arrivals.push(...(await page.evaluate(() => globalThis.recorder.drain())));
	// The RTT is read last: the PROBE minimum only falls as the session runs.
	const rtt = await page.evaluate(() => globalThis.recorder.info()?.rtt ?? null);

	const trimmed = arrivals.filter(([at]) => at - start <= durationMs);
	const round = (x: number, places: number) => Math.round(x * 10 ** places) / 10 ** places;
	const trace: Trace = {
		version: 1,
		source,
		description,
		transport: info.transport,
		rtt: rtt === null ? null : round(rtt, 1),
		config: info.config as Trace["config"],
		arrivals: trimmed.map(([at, timestamp, group]) => [round(at - start, 2), round(timestamp, 3), group]),
	};
	// One arrival per line, so the file reads as a table.
	const { arrivals: rows, ...header } = trace;
	const head = JSON.stringify(header, null, "\t").slice(0, -2);
	await Bun.write(out, `${head},\n\t"arrivals": [\n${rows.map((r) => `\t\t${JSON.stringify(r)}`).join(",\n")}\n\t]\n}\n`);
	console.log(
		`${out}: ${trace.arrivals.length} arrivals over ${round(durationMs / 1000, 1)} s, ${trace.config.codec} at ${trace.config.sampleRate} Hz, ${trace.transport}, rtt ${trace.rtt} ms`,
	);
} catch (err) {
	failed = true;
	console.error(err instanceof Error ? err.message : String(err));
} finally {
	await browser.close().catch(() => {});
	server.stop(true);
}
process.exit(failed ? 1 : 0);
