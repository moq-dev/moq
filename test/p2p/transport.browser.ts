/** Real RTCDataChannel SETUP and media transfer through the public MoQ API. @module */
import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type Browser, chromium } from "playwright";
import { build } from "vite";
import type { Report } from "./src/main.ts";

let dist: string;
let server: ReturnType<typeof Bun.serve>;
let browser: Browser;

beforeAll(async () => {
	dist = mkdtempSync(join(tmpdir(), "moq-p2p-"));
	await build({
		configFile: false,
		root: import.meta.dir,
		logLevel: "warn",
		build: { target: "esnext", outDir: dist, emptyOutDir: true },
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
	browser = await chromium.launch({ channel: "chromium", headless: true });
}, 120_000);

afterAll(async () => {
	await browser?.close();
	server?.stop();
	if (dist) rmSync(dist, { recursive: true, force: true });
});

test("two peer connections complete MoQ SETUP and transfer a frame without a relay", async () => {
	const page = await browser.newPage();
	const errors: string[] = [];
	page.on("pageerror", (error) => errors.push(error.message));
	try {
		await page.goto(`http://127.0.0.1:${server.port}/`);
		const report: Report = await page.evaluate(() => window.checkTransport());
		expect(report.client).toBe("webrtc");
		expect(report.server).toBe("webrtc");
		expect(report.version).toBe("moq-lite-07-wip");
		expect(report.bytes).toBe(256 * 1024);
		expect(report.peerStates).toEqual(["stable", "stable"]);
		expect(errors).toEqual([]);
	} finally {
		await page.close();
	}
}, 60_000);
