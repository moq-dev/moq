// TEMPORARY: CI stress loop for the P2P hang; not for merge.
import { afterAll, beforeAll, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { chromium } from "playwright";
import { build } from "vite";

let dist: string;
let server: ReturnType<typeof Bun.serve>;

beforeAll(async () => {
	dist = mkdtempSync(join(tmpdir(), "moq-p2p-"));
	await build({
		configFile: false,
		root: import.meta.dir,
		logLevel: "warn",
		build: { minify: false, target: "esnext", outDir: dist, emptyOutDir: true },
	});
	server = Bun.serve({
		port: 0,
		hostname: "127.0.0.1",
		async fetch(req) {
			const path = new URL(req.url).pathname;
			const file = Bun.file(join(dist, path === "/" ? "index.html" : path));
			return (await file.exists()) ? new Response(file) : new Response("not found", { status: 404 });
		},
	});
}, 120_000);

afterAll(() => {
	server?.stop();
	if (dist) rmSync(dist, { recursive: true, force: true });
});

test("stress", async () => {
	const N = Number(process.env.N ?? 200);
	const P = Number(process.env.P ?? 1);
	const budget = Date.now() + Number(process.env.BUDGET_MS ?? 20 * 60_000);
	let fails = 0;
	let runs = 0;
	let next = 0;
	const worker = async () => {
		while (next < N && Date.now() < budget) {
			const i = next++;
			runs++;
			const stderr: string[] = [];
			const b = await chromium.launch({
				channel: "chromium",
				headless: true,
				logger: { isEnabled: () => true, log: (_n, _s, m) => stderr.push(String(m)) },
			});
			const page = await b.newPage();
			const logs: string[] = [];
			page.on("console", (m) => logs.push(m.text()));
			page.on("pageerror", (e) => logs.push(`PAGEERROR ${e.message}`));
			let deadline: ReturnType<typeof setTimeout> | undefined;
			try {
				await page.goto(`http://127.0.0.1:${server.port}/`);
				const r = await Promise.race([
					page.evaluate(() => window.checkTransport()).then(
						() => "ok",
						(e) => `err ${e}`,
					),
					new Promise<string>((r) => {
						deadline = setTimeout(() => r("timeout"), 20_000);
					}),
				]);
				if (r !== "ok") {
					fails++;
					console.log(`=== ${i} ${r}\n${logs.join("\n")}\n--- browser\n${stderr.filter((l) => !l.includes("dbus")).join("\n")}`);
				}
			} finally {
				clearTimeout(deadline);
				await Promise.race([b.close(), new Promise((r) => setTimeout(r, 5000))]);
			}
		}
	};
	await Promise.all(Array.from({ length: P }, worker));
	console.log("fails", fails, "of", runs);
}, 3_000_000);
