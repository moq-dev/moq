// Drives a headless Chromium (channel "chromium" for WebTransport + WebCodecs)
// against a prebuilt page. publish streams a fake camera until killed; subscribe
// exits 0 once the watch element decodes a frame.
//
// --variant selects how the published packages are delivered to the page:
//   vite      bundled by vite           -> dist-vite/
//   esbuild   bundled by esbuild        -> dist-esbuild/
//   jsdelivr  imported from the CDN      -> jsdelivr/ (no build)
// The smoke harness builds the bundler variants first; jsdelivr needs no build.
//
//   bun driver.ts publish   --variant vite --url http://127.0.0.1:4443 --broadcast b.hang
//   bun driver.ts subscribe --variant jsdelivr --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
import { join } from "node:path";
import { parseArgs } from "node:util";
import { chromium } from "playwright";
import { Diagnostics } from "./diagnostics";

const { positionals, values } = parseArgs({
	allowPositionals: true,
	options: {
		url: { type: "string" },
		broadcast: { type: "string" },
		variant: { type: "string", default: "vite" },
		timeout: { type: "string", default: "20" },
	},
});

const role = positionals[0];
const url = values.url;
const broadcast = values.broadcast;
const variant = values.variant ?? "vite";
const dirByVariant: Record<string, string> = {
	vite: "dist-vite",
	esbuild: "dist-esbuild",
	jsdelivr: "jsdelivr",
};
const dir = dirByVariant[variant];
if ((role !== "publish" && role !== "subscribe") || !url || !broadcast || !dir) {
	console.error(
		"usage: driver.ts publish|subscribe --variant vite|esbuild|jsdelivr --url U --broadcast B [--timeout S]",
	);
	process.exit(2);
}
const timeoutMs = Number.parseFloat(values.timeout ?? "20") * 1000;

// Serve the prebuilt page on localhost (a secure context, so WebTransport /
// WebCodecs are enabled). A plain static server avoids running concurrent Vite
// dev servers, which deadlock on the shared dep-optimizer cache.
const root = join(new URL(".", import.meta.url).pathname, dir);
const server = Bun.serve({
	port: 0,
	async fetch(req) {
		let path = new URL(req.url).pathname;
		if (path === "/") path = "/index.html";
		const file = Bun.file(join(root, path));
		if (await file.exists()) return new Response(file);
		return new Response(Bun.file(join(root, "index.html"))); // SPA fallback
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
let error: unknown;
try {
	const context = await browser.newContext();
	const page = await context.newPage();
	const diagnostics = new Diagnostics(page, role, variant, pageUrl);
	const artifactDir = process.env.SMOKE_BROWSER_ARTIFACTS;
	const artifactName = [role, variant, broadcast].join("-").replaceAll(/[^A-Za-z0-9._-]/g, "_");
	if (artifactDir) await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
	// The published elements race a WebSocket fallback and expose no switch for it.
	// smoke.sh sets this on external relays that are not the WebSocket column, so
	// the page cannot open one. A ws(s) URL leaves the constructor alone.
	if (process.env.SMOKE_WEBSOCKET === "0") {
		await page.addInitScript(`
			const dead = function WebSocket() {
				throw new Error("WebSocket is disabled for this relay");
			};
			globalThis.WebSocket = dead;
			globalThis.WebSocketStream = dead;
		`);
	}
	page.on("console", (m) => console.error(`[page] ${m.text()}`));
	page.on("pageerror", (e) => console.error(`[page error] ${e.message}`));
	// smoke.sh SIGKILLs a publisher, which skips the artifact writes below, so a
	// publisher whose scripts fail to load exits here instead of streaming nothing.
	const scriptFailed = new Promise<never>((_, reject) => {
		page.on("requestfailed", (r) => {
			if (r.resourceType() === "script")
				reject(new Error(`script failed: ${r.url()}: ${r.failure()?.errorText}`));
		});
		page.on("response", (r) => {
			if (r.request().resourceType() === "script" && r.status() >= 400) {
				reject(new Error(`script failed: ${r.url()}: HTTP ${r.status()}`));
			}
		});
	});
	scriptFailed.catch(() => {}); // only the publisher awaits it
	try {
		await page.goto(pageUrl, { waitUntil: "load" });

		if (role === "publish") {
			console.error(`publishing ${broadcast} (fake camera, ${variant}) to ${url}`);
			await scriptFailed; // otherwise stream until the orchestrator kills us
		} else {
			const deadline = Date.now() + timeoutMs;
			let frames = 0;
			while (Date.now() < deadline) {
				frames = await page.evaluate(() => {
					const w = document.querySelector("moq-watch") as unknown as {
						video?: { out?: { stats?: { peek(): { frameCount?: number } | undefined } } };
					} | null;
					return w?.video?.out?.stats?.peek()?.frameCount ?? 0;
				});
				if (frames > 0) break;
				await new Promise((r) => setTimeout(r, 250));
			}
			console.error(`decoded ${frames} frames from ${broadcast} (${variant})`);
			code = frames > 0 ? 0 : 1;
		}
	} catch (caught) {
		error = caught;
	} finally {
		if (artifactDir) {
			if (code === 0) await context.tracing.stop();
			else {
				// Save both artifacts even if one fails, and keep the run error as the cause.
				await diagnostics
					.write(artifactDir, artifactName, error)
					.catch((e) => console.error("diagnostics write failed:", e));
				await context.tracing
					.stop({ path: join(artifactDir, `${artifactName}.zip`) })
					.catch((e) => console.error("trace save failed:", e));
			}
		}
	}
} finally {
	await browser.close().catch(() => {});
	server.stop(true);
}
if (error) throw error;
process.exit(code);
