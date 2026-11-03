/** Run networking microbenchmarks in warmed Chromium pages. */
import { resolve } from "node:path";
import { parseArgs } from "node:util";
import { chromium, type Page } from "playwright";
import { collections, GC_CATEGORIES, rendererPid, traceEvents } from "./trace.ts";
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
// The rows each benchmark prints per run.
const ROWS: Record<string, number> = {
	frames: 26,
	drift: 36,
	ledger: 12,
	bandwidth: 44,
	replay: 72,
	"resume-history": 72,
	buffered: 36,
};
if (!Object.hasOwn(ROWS, benchmark)) throw new Error(`--bench must be one of ${Object.keys(ROWS).join(", ")}`);
// The fixture directory, relative to each checkout; buffered drives @moq/hang's container consumer.
const dir = benchmark === "buffered" ? "js/hang/bench" : "js/net/bench";
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
	const entry =
		benchmark === "frames"
			? resolve(root, dir, "frames.ts")
			: resolve(import.meta.dir, "../..", dir, `${benchmark}.ts`);
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
						importer.endsWith(`/${dir}/${benchmark}.ts`) ? { path: resolve(root, dir, path) } : undefined,
					);
					build.onLoad(
						{ filter: /\/bench\/(frames|drift|ledger|bandwidth|replay|resume-history|buffered)\.ts$/ },
						async ({ path }) => {
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
						},
					);
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
		// Cross-origin isolated, so performance.now() resolves 5 us instead of 100 us.
		return new Response(`<script type="module" src="/${name}.js"></script>`, {
			headers: {
				"Content-Type": "text/html",
				"Cross-Origin-Opener-Policy": "same-origin",
				"Cross-Origin-Embedder-Policy": "require-corp",
			},
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
const traced: { variant: string; rows: string[] }[] = [];
// Main-thread GC across one whole run per variant, from a traced pass after the timed rounds.
const gc: Record<string, ReturnType<typeof collections>> = {};
const save = () =>
	Bun.write(
		values.out,
		JSON.stringify(
			{ benchmark, browser: browser.version(), warmup: 1, roots: Object.fromEntries(roots), samples, traced, gc },
			null,
			2,
		),
	);
try {
	const pages = new Map<string, Page>();
	const rows = new Map<string, string[]>();
	for (const name of roots.keys()) {
		const page = await browser.newPage();
		pages.set(name, page);
		rows.set(name, []);
		page.on("console", (message) => {
			if (/^(lite|ietf|steady|lookahead|sparse|burst|\d+),/.test(message.text()))
				rows.get(name)?.push(message.text());
		});
		page.on("pageerror", (error) => {
			console.error(error);
			void page.close();
		});
		await page.goto(`http://127.0.0.1:${server.port}/${name}`);
		if (!(await page.evaluate("crossOriginIsolated"))) throw new Error(`${name} page is not cross-origin isolated`);
		await page.waitForFunction(() => typeof (globalThis as unknown as { measure: unknown }).measure === "function");
		await page.evaluate("globalThis.measure()");
		console.log(`warmed ${name}`);
	}
	// One run of `variant`'s page, requiring every row.
	const measure = async (variant: string) => {
		rows.set(variant, []);
		await pages.get(variant)?.evaluate("globalThis.measure()");
		const measured = rows.get(variant) ?? [];
		if (measured.length !== ROWS[benchmark]) throw new Error(`incomplete rows: ${variant}`);
		return measured;
	};
	for (let round = 0; round < rounds; round++) {
		const names = [...roots.keys()];
		if (round % 2) names.reverse();
		for (const variant of names) {
			samples.push({ round, variant, rows: await measure(variant) });
			await save();
			console.log(`measured ${round} ${variant}`);
		}
	}
	// Traced apart from the timed rounds, so tracing overhead never enters a timing.
	for (const [variant, page] of pages) {
		if (benchmark !== "replay" && benchmark !== "resume-history") continue;
		await browser.startTracing(page, { categories: GC_CATEGORIES });
		let buffer: Buffer | undefined;
		try {
			traced.push({ variant, rows: await measure(variant) });
		} finally {
			buffer = await browser.stopTracing();
		}
		const events = traceEvents(buffer);
		gc[variant] = collections(events, rendererPid(events, page.url()));
		await save();
		console.log(`traced ${variant}`);
	}
	// Reachability is exact here, so a retired route still reachable fails the run, once every
	// variant's samples are written for comparison.
	if (benchmark === "resume-history") {
		const retaining = [...samples, ...traced].filter(({ rows }) =>
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
