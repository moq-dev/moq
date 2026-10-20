/**
 * Run the UniFFI cases in headless Chromium, once with the player's `new Function`
 * dispatchers and once under a strict CSP that forces its interpreter.
 *
 * Writes `uniffi.json` into the harness run directory. Fails on any failed case.
 */
import { join } from "node:path";
import { parseArgs } from "node:util";
import { chromium } from "playwright";
import type { Relay, Report } from "./cases.ts";

// `--timeout` bounds each page, init and cases and bench together, in seconds. Each case
// carries its own 15 s bound in the page; this catches a hang outside them.
const { values } = parseArgs({ options: { relays: { type: "string" }, timeout: { type: "string", default: "300" } } });
const run = process.env.MOQ_TEST_RUN;
const timeout = Number(values.timeout) * 1000;
if (!values.relays || !run || !Number.isFinite(timeout) || timeout <= 0)
	throw new Error("needs --relays, MOQ_TEST_RUN, and a positive --timeout");
const relays = (await Bun.file(values.relays).json()) as Relay[];
const here = new URL(".", import.meta.url).pathname;
const dist = join(here, "../dist");

// No 'unsafe-eval': the player's registration probe fails and it interprets instead.
const STRICT = "script-src 'self' 'wasm-unsafe-eval'";

const server = Bun.serve({
	hostname: "127.0.0.1",
	port: 0,
	async fetch(req) {
		const url = new URL(req.url);
		if (url.pathname === "/")
			return new Response(`<script type="module" src="/uniffi-cases.js"></script>`, {
				headers: {
					"content-type": "text/html",
					"cache-control": "no-store",
					...(url.searchParams.has("strict") ? { "content-security-policy": STRICT } : {}),
				},
			});
		const file = url.pathname.startsWith("/uniffi/")
			? Bun.file(join(here, "dist", url.pathname.slice(8)))
			: Bun.file(join(dist, url.pathname));
		return (await file.exists())
			? new Response(file, { headers: { "cache-control": "no-store" } })
			: new Response("not found", { status: 404 });
	},
});

const browser = await chromium.launch({ channel: "chromium", headless: true });
const version = browser.version();
const reports: Record<string, Report> = {};
let failed = 0;
try {
	for (const mode of ["jit", "strict"] as const) {
		const context = await browser.newContext();
		const page = await context.newPage();
		const errors: string[] = [];
		page.on("pageerror", (error) => errors.push(error.message));
		page.on("console", (message) => {
			if (message.text().includes("panicked at")) errors.push(`panic: ${message.text()}`);
		});
		await page.goto(`http://127.0.0.1:${server.port}/${mode === "strict" ? "?strict" : ""}`);
		await page.waitForFunction(() => typeof window.uniffiCases === "function");
		const report = await bounded(
			page.evaluate((value) => window.uniffiCases(value), relays),
			mode,
		);
		if (errors.length) throw new Error(`${mode}: ${errors.join("; ")}`);
		reports[mode] = report;
		if (report.eval !== (mode === "jit")) throw new Error(`${mode}: new Function allowed = ${report.eval}`);
		console.log(`── ${mode}: init ${report.init_ms.toFixed(1)}ms`);
		for (const result of report.cases) {
			if (!result.ok) failed++;
			const detail = result.ok ? (result.note ?? "") : result.error;
			console.log(`  ${result.ok ? "ok  " : "FAIL"} ${result.name}${detail ? ` -- ${detail}` : ""}`);
		}
		for (const bench of report.bench) console.log(`  ${bench.us_per_op.toFixed(2).padStart(9)} us  ${bench.name}`);
		await context.close();
	}
} finally {
	await browser.close();
	server.stop(true);
	await Bun.write(join(run, "uniffi.json"), JSON.stringify({ browser: version, reports }, null, 2));
}
if (failed) {
	console.error(`${failed} UniFFI case(s) failed`);
	process.exit(1);
}

async function bounded<T>(task: Promise<T>, mode: string): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			task,
			new Promise<never>((_resolve, reject) => {
				timer = setTimeout(
					() => reject(new Error(`${mode}: page did not finish in ${timeout / 1000}s`)),
					timeout,
				);
			}),
		]);
	} finally {
		clearTimeout(timer);
	}
}
