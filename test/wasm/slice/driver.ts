/**
 * Run the slice bench in headless Chromium, alternating generators across fresh pages so
 * drift on a shared host hits both. Writes `slice.json` into the harness run directory.
 */
import { join } from "node:path";
import { chromium } from "playwright";
import type { Report } from "./bench.ts";

const run = process.env.MOQ_TEST_RUN;
if (!run) throw new Error("needs MOQ_TEST_RUN");
const rounds = Number(process.env.MOQ_SLICE_ROUNDS ?? 6);
const here = new URL(".", import.meta.url).pathname;
const dist = join(here, "../dist");
const modules: Record<string, string> = {
	"/slice/uniffi.wasm": join(here, "dist/uniffi/moq_ffi_gen.wasm"),
	"/slice/boltffi.wasm": join(here, "dist/boltffi/moq_ffi_gen_bg.wasm"),
};

const server = Bun.serve({
	hostname: "127.0.0.1",
	port: 0,
	async fetch(req) {
		const path = new URL(req.url).pathname;
		if (path === "/")
			return new Response(`<script type="module" src="/slice-bench.js"></script>`, {
				headers: { "content-type": "text/html", "cache-control": "no-store" },
			});
		const file = Bun.file(modules[path] ?? join(dist, path));
		return (await file.exists())
			? new Response(file, { headers: { "cache-control": "no-store" } })
			: new Response("not found", { status: 404 });
	},
});

const browser = await chromium.launch({ channel: "chromium", headless: true });
const version = browser.version();
const reports: Report[] = [];
try {
	for (let round = 0; round < rounds; round++) {
		// ABBA order: each generator goes first, and twice in a row, equally often.
		const order = round % 2 ? ["boltffi", "uniffi"] : ["uniffi", "boltffi"];
		for (const generator of order) {
			const context = await browser.newContext();
			const page = await context.newPage();
			const errors: string[] = [];
			page.on("pageerror", (error) => errors.push(error.message));
			await page.goto(`http://127.0.0.1:${server.port}/`);
			await page.waitForFunction(() => typeof window.sliceBench === "function");
			const report = await page.evaluate((value) => window.sliceBench(value), generator);
			if (errors.length) throw new Error(`${generator}: ${errors.join("; ")}`);
			reports.push(report);
			console.log(`round ${round} ${generator}: init ${report.init_ms.toFixed(1)}ms`);
			await context.close();
		}
	}
} finally {
	await browser.close();
	server.stop(true);
	await Bun.write(join(run, "slice.json"), JSON.stringify({ browser: version, reports }, null, 2));
}

// Median per case and generator, with the range, as the report tables show them.
const cases = new Map<string, Map<string, number[]>>();
for (const report of reports)
	for (const bench of report.bench) {
		const byGenerator = cases.get(bench.name) ?? new Map<string, number[]>();
		byGenerator.set(report.generator, [...(byGenerator.get(report.generator) ?? []), bench.us_per_op]);
		cases.set(bench.name, byGenerator);
	}
const median = (values: number[]) => {
	const sorted = [...values].sort((a, b) => a - b);
	const middle = Math.floor(sorted.length / 2);
	return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
const cell = (values: number[] = []) =>
	values.length
		? `${median(values).toFixed(2)} (${Math.min(...values).toFixed(2)}-${Math.max(...values).toFixed(2)})`
		: "-";
console.log("| case | UniFFI us | BoltFFI us |\n|---|---|---|");
for (const [name, byGenerator] of cases)
	console.log(`| ${name} | ${cell(byGenerator.get("uniffi"))} | ${cell(byGenerator.get("boltffi"))} |`);
