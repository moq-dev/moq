/** Run networking microbenchmarks in warmed Chromium pages. */
import { resolve } from "node:path";
import { parseArgs } from "node:util";
import { chromium, type Page } from "playwright";
import { workspace } from "./workspace.ts";

const { values, positionals } = parseArgs({
	args: Bun.argv.slice(2),
	allowPositionals: true,
	options: {
		bench: { type: "string", default: "frames" },
		out: { type: "string", default: ".tmp/browser-micro.json" },
		rounds: { type: "string", default: "3" },
	},
});
const benchmark = values.bench;
// Each entry is named in full, so sh/gh/test-inventory.py sees this harness run it.
const benchmarks: Record<string, { entry: string; rows: number }> = {
	frames: { entry: "js/net/bench/frames.ts", rows: 26 },
	drift: { entry: "js/net/bench/drift.ts", rows: 36 },
	ledger: { entry: "js/net/bench/ledger.ts", rows: 12 },
	"resume-history": { entry: "js/net/bench/resume-history.ts", rows: 72 },
};
const selected = Object.hasOwn(benchmarks, benchmark) ? benchmarks[benchmark] : undefined;
if (!selected) throw new Error(`--bench must be one of: ${Object.keys(benchmarks).join(", ")}`);
const expectedRows = selected.rows;
const rounds = Number(values.rounds);
if (!Number.isSafeInteger(rounds) || rounds < 1) throw new Error("--rounds must be a positive integer");
const roots = new Map<string, string>();
for (const argument of positionals.length ? positionals : ["current=.", "aa=."]) {
	const separator = argument.indexOf("=");
	const name = argument.slice(0, separator);
	if (separator < 1 || !/^[a-z]+$/.test(name) || roots.has(name)) throw new Error(`invalid variant: ${argument}`);
	roots.set(name, resolve(argument.slice(separator + 1)));
}
const scripts = new Map<string, string>();
for (const [name, root] of roots) {
	// Only frames is bundled from each variant; the rest import each variant's `../src/` below.
	const entry = resolve(benchmark === "frames" ? root : resolve(import.meta.dir, "../.."), selected.entry);
	const result = await Bun.build({
		entrypoints: [entry],
		target: "browser",
		plugins: [
			workspace(root),
			{
				name: "repeat-microbenchmark",
				setup(build) {
					// Internal modules have no package export, so select each variant's own explicitly.
					build.onResolve({ filter: /^\.\.\/src\// }, ({ path, importer }) =>
						importer.endsWith(`/js/net/bench/${benchmark}.ts`)
							? { path: resolve(root, "js/net/bench", path) }
							: undefined,
					);
					build.onLoad({ filter: /\/bench\/(frames|drift|ledger|resume-history)\.ts$/ }, async ({ path }) => {
						const source = await Bun.file(path).text();
						if (benchmark !== "frames")
							return { loader: "ts", contents: `${source}\nglobalThis.measure = run;` };
						const marker =
							'console.log("protocol,frame_bytes,chunk_bytes,frames_per_chunk,ns_per_frame,first_frame_us");';
						const parts = source.split(marker);
						if (parts.length !== 2) throw new Error("frames benchmark entry moved");
						return {
							loader: "ts",
							contents: `${parts[0]}globalThis.measure = async () => {${marker}\n${parts[1]}\n};`,
						};
					});
				},
			},
		],
	});
	if (!result.success) throw new AggregateError(result.logs, `bundle failed: ${name}`);
	scripts.set(name, await result.outputs[0].text());
}
if (scripts.has("aa") && scripts.get("aa") !== scripts.get("current")) throw new Error("A/A bundles differ");
const server = Bun.serve({
	port: 0,
	fetch(request) {
		const name = new URL(request.url).pathname.slice(1);
		if (name.endsWith(".js")) {
			const script = scripts.get(name.slice(0, -3));
			return new Response(script ?? "unknown variant", {
				status: script === undefined ? 404 : 200,
				headers: { "Content-Type": "text/javascript" },
			});
		}
		if (!scripts.has(name)) return new Response("unknown variant", { status: 404 });
		return new Response(`<script type="module" src="/${name}.js"></script>`, {
			headers: { "Content-Type": "text/html" },
		});
	},
});
const browser = await chromium.launch({
	headless: true,
	executablePath: process.env.BROWSER_BIN,
	// Retention is counted after full collections the page asks for, against an unrounded heap size.
	args: benchmark === "resume-history" ? ["--js-flags=--expose-gc", "--enable-precise-memory-info"] : [],
});
const samples: { round: number; variant: string; rows: string[] }[] = [];
try {
	const pages = new Map<string, Page>();
	const rows = new Map<string, string[]>();
	for (const name of roots.keys()) {
		const page = await browser.newPage();
		pages.set(name, page);
		rows.set(name, []);
		page.on("console", (message) => {
			if (/^(lite|ietf|\d+),/.test(message.text())) rows.get(name)?.push(message.text());
		});
		page.on("pageerror", (error) => {
			console.error(error);
			void page.close();
		});
		await page.goto(`http://127.0.0.1:${server.port}/${name}`);
		await page.waitForFunction(() => typeof (globalThis as unknown as { measure: unknown }).measure === "function");
		await page.evaluate("globalThis.measure()");
		console.log(`warmed ${name}`);
	}
	for (let round = 0; round < rounds; round++) {
		const names = [...roots.keys()];
		if (round % 2) names.reverse();
		for (const variant of names) {
			rows.set(variant, []);
			await pages.get(variant)?.evaluate("globalThis.measure()");
			const measured = rows.get(variant);
			if (measured?.length !== expectedRows) throw new Error(`incomplete rows: ${variant}`);
			samples.push({ round, variant, rows: measured });
			await Bun.write(
				values.out,
				JSON.stringify(
					{ benchmark, browser: browser.version(), warmup: 1, roots: Object.fromEntries(roots), samples },
					null,
					2,
				),
			);
			console.log(`measured ${round} ${variant}`);
		}
	}
	// Reachability is exact here, so a retired route still reachable fails the run, once every
	// variant's samples are written for comparison.
	if (benchmark === "resume-history") {
		const retaining = samples.filter(({ rows }) =>
			rows.some((row) =>
				row
					.split(",")
					.slice(5, 9)
					.some((retired) => retired !== "0"),
			),
		);
		if (retaining.length)
			throw new Error(
				`retired routes stayed reachable: ${[...new Set(retaining.map((s) => s.variant))].join(", ")}`,
			);
	}
} finally {
	await browser.close();
	server.stop(true);
}
