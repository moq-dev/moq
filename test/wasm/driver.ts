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
 * @module
 */
import { join } from "node:path";
import { parseArgs } from "node:util";
import { chromium } from "playwright";
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

let code = 1;
try {
	const page = await browser.newPage();

	// A Rust panic reaches the console through `console_error_panic_hook` rather
	// than rejecting anything, so it can leave every case green. Fail on it, and
	// on an uncaught exception, independently of the case results.
	const fatal: string[] = [];
	page.on("console", (message) => {
		const text = message.text();
		console.error(`[page] ${text}`);
		if (text.includes("panicked at")) fatal.push(`panic: ${text}`);
	});
	page.on("pageerror", (error) => {
		console.error(`[page error] ${error.message}`);
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
	await browser.close().catch(() => {});
	server.stop(true);
}

process.exit(code);
