/**
 * Drives the `@moq/wasm` harness page in headless Chromium.
 *
 * Serves the bundled page plus the generated `js/wasm/dist` next to it, opens
 * one tab, evaluates `moqWasmTest(config)`, and prints what each case reported.
 * Relays are started by `run.sh`, which writes their URLs and expected versions
 * into the `--relays` file.
 *
 *     bun driver.ts --relays relays.json [--timeout 30]
 *
 * A failing run leaves a Playwright trace, a screenshot, and a HAR in
 * $MOQ_QA_TRACE, which the debug bundle sets (see test/lib/bundle.sh).
 *
 * @module
 */
import { rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { chromium, type Page } from "playwright";
import type { CaseResult, Config, RelayFixture } from "./src/main.ts";

const { values } = parseArgs({
	options: {
		relays: { type: "string" },
		timeout: { type: "string", default: "30" },
	},
});

const timeoutMs = Number.parseFloat(values.timeout ?? "30") * 1000;
if (!values.relays || !Number.isFinite(timeoutMs) || timeoutMs <= 0) {
	console.error("usage: driver.ts --relays relays.json [--timeout S>0]");
	process.exit(2);
}

const relays = (await Bun.file(values.relays).json()) as RelayFixture[];
if (relays.length === 0) {
	console.error("error: no relays to test against");
	process.exit(2);
}

const here = new URL(".", import.meta.url).pathname;
const dist = join(here, "dist");
const wasm = join(here, "../../js/wasm/dist");

// localhost is a secure context, so WebTransport is available without TLS here.
// `/wasm/` serves the generated bindings unmodified; see src/main.ts.
const server = Bun.serve({
	port: 0,
	hostname: "127.0.0.1",
	async fetch(req) {
		const path = new URL(req.url).pathname;
		const file = path.startsWith("/wasm/")
			? Bun.file(join(wasm, path.slice("/wasm/".length)))
			: Bun.file(join(dist, path === "/" ? "index.html" : path));
		if (!(await file.exists())) return new Response("not found", { status: 404 });
		return new Response(file);
	},
});

const config: Config = {
	module: `http://127.0.0.1:${server.port}/wasm/moq.js`,
	relays,
	timeout: timeoutMs,
};

const browser = await chromium.launch({
	channel: "chromium", // full Chromium (new headless); the headless shell lacks WebTransport
	headless: true,
});

const traceDir = process.env.MOQ_QA_TRACE;
const label = process.env.MOQ_QA_LABEL ?? "wasm";

// The trace carries the DOM snapshots, screenshots, and page errors a case
// result cannot. HAR bodies are omitted: the session runs over WebTransport,
// which neither the trace nor the HAR can see (that is what relay qlog is for),
// so the bodies would be page assets and nothing else.
const context = await browser.newContext(
	traceDir ? { recordHar: { path: join(traceDir, `${label}.har`), content: "omit" } } : {},
);
if (traceDir) {
	await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
}

// A trace is worth its megabytes only for a run that failed; a passing one
// discards everything it recorded.
async function saveTrace(page: Page | undefined, failed: boolean, log: string[]): Promise<void> {
	if (!traceDir) {
		await context.close().catch(() => {});
		return;
	}
	if (failed) {
		await context.tracing.stop({ path: join(traceDir, `${label}.trace.zip`) }).catch(() => {});
		await page?.screenshot({ path: join(traceDir, `${label}.png`), fullPage: true }).catch(() => {});
		await writeFile(join(traceDir, `${label}.console.log`), `${log.join("\n")}\n`).catch(() => {});
	} else {
		await context.tracing.stop().catch(() => {});
	}
	// The HAR is only written on close, so it has to happen either way.
	await context.close().catch(() => {});
	if (!failed) await rm(join(traceDir, `${label}.har`), { force: true }).catch(() => {});
}

// Every line the page printed, kept whether or not it was fatal: a case that
// failed on a timeout usually explains itself in the lines before it.
const transcript: string[] = [];

let code = 1;
let page: Page | undefined;
try {
	page = await context.newPage();

	// A Rust panic reaches the console through `console_error_panic_hook` rather
	// than rejecting anything, so it can leave every case green. Fail on it, and
	// on an uncaught exception, independently of the case results.
	const fatal: string[] = [];
	page.on("console", (message) => {
		const text = message.text();
		console.error(`[page] ${text}`);
		transcript.push(`page: ${text}`);
		if (text.includes("panicked at")) fatal.push(`panic: ${text}`);
	});
	page.on("pageerror", (error) => {
		console.error(`[page error] ${error.message}`);
		transcript.push(`pageerror: ${error.message}`);
		fatal.push(`uncaught: ${error.message}`);
	});

	await page.goto(`http://127.0.0.1:${server.port}/`, { waitUntil: "load" });

	// The page bounds each case itself, so this only covers a wedge outside that
	// loop: the wasm module never loading, or evaluate never returning. Generous
	// on purpose, since the per-case budget is the tight one and a whole suite is
	// seconds. Without it the job would sit until the workflow's own timeout.
	const suiteTimeoutMs = timeoutMs * 10;
	const results: CaseResult[] = await Promise.race([
		page.evaluate((cfg) => window.moqWasmTest(cfg), config),
		new Promise<never>((_resolve, reject) =>
			setTimeout(() => reject(new Error(`suite did not finish within ${suiteTimeoutMs}ms`)), suiteTimeoutMs),
		),
	]);

	let failed = 0;
	let known = 0;
	for (const result of results) {
		if (result.ok && !result.known) {
			console.log(`  ok    ${result.name}`);
		} else if (result.ok) {
			// The bug it was waiting on is fixed, so the marker is now the lie.
			failed++;
			console.log(`  FAIL  ${result.name}: passes now; drop the ${result.known} marker`);
		} else if (result.known) {
			known++;
			console.log(`  known ${result.name} (${result.known}): ${result.error}`);
		} else {
			failed++;
			console.log(`  FAIL  ${result.name}: ${result.error}`);
		}
	}
	for (const message of fatal) console.log(`  FAIL  ${message}`);

	const total = results.length;
	const summary = [`${total - failed - known}/${total} cases passed`];
	if (known > 0) summary.push(`${known} known`);
	if (fatal.length > 0) summary.push(`${fatal.length} fatal`);
	console.log(summary.join(", "));
	code = failed === 0 && fatal.length === 0 ? 0 : 1;
} catch (err) {
	console.log(`  FAIL  ${err instanceof Error ? err.message : String(err)}`);
} finally {
	await saveTrace(page, code !== 0, transcript);
	await browser.close().catch(() => {});
	server.stop(true);
}

if (code !== 0 && traceDir) console.error(`browser trace: ${join(traceDir, `${label}.trace.zip`)}`);
process.exit(code);
