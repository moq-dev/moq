// Drives a headless Chromium (nix-provided, channel "chromium" for WebTransport
// + WebCodecs) against the prebuilt page in dist/. publish streams a fake camera
// until killed; subscribe exits 0 once the watch element decodes a frame.
//
// Build the page once first: `vite build` (the smoke harness does this).
//
//   bun driver.ts publish   --url http://127.0.0.1:4443 --broadcast b.hang
//   bun driver.ts subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
import { join } from "node:path";
import { parseArgs } from "node:util";
import { chromium } from "playwright";

const { positionals, values } = parseArgs({
	allowPositionals: true,
	options: {
		url: { type: "string" },
		broadcast: { type: "string" },
		timeout: { type: "string", default: "20" },
	},
});

const role = positionals[0];
const url = values.url;
const broadcast = values.broadcast;
if ((role !== "publish" && role !== "subscribe") || !url || !broadcast) {
	console.error("usage: driver.ts publish|subscribe --url U --broadcast B [--timeout S]");
	process.exit(2);
}
const timeoutMs = Number.parseFloat(values.timeout ?? "20") * 1000;

// Serve the prebuilt page on localhost (a secure context, so WebTransport /
// WebCodecs are enabled). A plain static server avoids running concurrent Vite
// dev servers, which deadlock on the shared dep-optimizer cache.
const dist = join(new URL(".", import.meta.url).pathname, "dist");
const server = Bun.serve({
	port: 0,
	async fetch(req) {
		let path = new URL(req.url).pathname;
		if (path === "/") path = "/index.html";
		const file = Bun.file(join(dist, path));
		if (await file.exists()) return new Response(file);
		return new Response(Bun.file(join(dist, "index.html"))); // SPA fallback
	},
});
const pageUrl = `http://localhost:${server.port}/?role=${role}&url=${encodeURIComponent(url)}&broadcast=${encodeURIComponent(broadcast)}`;

const browser = await chromium.launch({
	channel: "chromium", // full Chromium (new headless); the headless shell lacks these APIs
	headless: true,
	args: [
		"--use-fake-device-for-media-stream",
		"--use-fake-ui-for-media-stream",
		"--autoplay-policy=no-user-gesture-required",
	],
});

let code = 1;
try {
	const page = await browser.newPage();
	page.on("console", (m) => console.error(`[page] ${m.text()}`));
	page.on("pageerror", (e) => console.error(`[page error] ${e.message}`));
	await page.goto(pageUrl, { waitUntil: "load" });

	if (role === "publish") {
		console.error(`publishing ${broadcast} (fake camera) to ${url}`);
		await new Promise(() => {}); // stream until the orchestrator kills us
	} else {
		const start = Date.now();
		const deadline = start + timeoutMs;
		let frames = 0;
		let reloaded = false;
		while (Date.now() < deadline) {
			frames = await page.evaluate(() => {
				const w = document.querySelector("moq-watch") as unknown as {
					backend?: { video?: { stats?: { peek(): { frameCount?: number } | undefined } } };
				} | null;
				return w?.backend?.video?.stats?.peek()?.frameCount ?? 0;
			});
			if (frames > 0) break;
			// The watch element gives up if it subscribes to the catalog before the
			// publisher has announced it (RESET_STREAM). If nothing has decoded by the
			// halfway mark, reload once to re-subscribe now that the publisher is up.
			if (!reloaded && Date.now() - start > timeoutMs / 2) {
				reloaded = true;
				await page.reload({ waitUntil: "load" }).catch(() => {});
			}
			await new Promise((r) => setTimeout(r, 250));
		}
		console.error(`decoded ${frames} frames from ${broadcast}`);
		code = frames > 0 ? 0 : 1;
	}
} finally {
	await browser.close().catch(() => {});
	server.stop(true);
}
process.exit(code);
