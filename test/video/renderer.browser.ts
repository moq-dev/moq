/**
 * The `@moq/video` renderer in headless Chromium: paint through WebGPU, lose the device with no
 * adapter to replace it, check the renderer reports `"surface-lost"`, then check a fresh canvas
 * paints through Canvas2D. Once against the renderer directly and once through `<moq-publish>`,
 * which swaps its own canvas.
 *
 * Not `*.test.ts`: plain `bun test` runs lack Playwright Chromium, which `just test video` installs.
 *
 * @module
 */
import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type Browser, chromium, type Page } from "playwright";
import { build } from "vite";
import { worklet } from "../../js/common/vite-plugin-worklet";
import type { ElementReport, RendererReport } from "./src/main";

declare global {
	interface Window {
		readonly activeObservers: number;
	}
}

// SwiftShader stands in for a GPU, so this runs the same with or without one. The fake camera
// feeds `<moq-publish source="camera">` without a prompt.

const ARGS = [
	"--enable-unsafe-webgpu",
	"--enable-features=Vulkan",
	"--use-vulkan=swiftshader",
	"--use-gl=angle",
	"--use-angle=swiftshader",
	"--use-fake-device-for-media-stream",
	"--use-fake-ui-for-media-stream",
];

let dist: string;
let server: ReturnType<typeof Bun.serve>;
let browser: Browser;

beforeAll(async () => {
	dist = mkdtempSync(join(tmpdir(), "moq-video-"));
	await build({
		configFile: false,
		root: import.meta.dir,
		logLevel: "warn",
		plugins: [worklet()],
		build: { target: "esnext", outDir: dist, emptyOutDir: true },
	});

	// WebGPU needs a secure context, which a loopback origin is.
	server = Bun.serve({
		port: 0,
		hostname: "127.0.0.1",
		async fetch(req) {
			const path = new URL(req.url).pathname;
			const file = Bun.file(join(dist, path === "/" ? "index.html" : path));
			return (await file.exists()) ? new Response(file) : new Response("not found", { status: 404 });
		},
	});

	browser = await chromium.launch({ channel: "chromium", headless: true, args: ARGS });
}, 120_000);

afterAll(async () => {
	await browser?.close();
	server?.stop();
	if (dist) rmSync(dist, { recursive: true, force: true });
});

async function open(): Promise<Page> {
	const page = await browser.newPage();
	await page.addInitScript(() => {
		const active = new Set<ResizeObserver>();
		const Original = window.ResizeObserver;
		window.ResizeObserver = class extends Original {
			override observe(...args: Parameters<ResizeObserver["observe"]>) {
				active.add(this);
				super.observe(...args);
			}
			override disconnect() {
				active.delete(this);
				super.disconnect();
			}
		};
		Object.defineProperty(window, "activeObservers", { get: () => active.size });
	});
	page.on("console", (message) => console.error(`[page] ${message.text()}`));
	page.on("pageerror", (error) => console.error(`[page error] ${error.message}`));
	await page.goto(`http://127.0.0.1:${server.port}/`, { waitUntil: "load" });
	return page;
}

test("the renderer reports a lost surface and recovers on a fresh canvas through Canvas2D", async () => {
	const page = await open();
	try {
		const report: RendererReport = await page.evaluate(() => window.checkRenderer());

		// Both paths show the same rotation: the frame's left (red) half on top.
		expect(report.before).toEqual({ context: "webgpu", colors: ["red", "blue"] });
		expect(report.error).toBe("surface-lost");
		expect(report.after).toEqual({ context: "2d", colors: ["red", "blue"] });
		expect(report.recovered).toBeUndefined();
	} finally {
		await page.close();
	}
}, 60_000);

test("<moq-publish> swaps its canvas when the GPU is lost and keeps previewing through Canvas2D", async () => {
	const page = await open();
	try {
		const report: ElementReport = await page.evaluate(() => window.checkElement());

		expect(report.before.context).toBe("webgpu");
		expect(report.after.context).toBe("2d");
	} finally {
		await page.close();
	}
}, 60_000);

test("invalid player and publisher attributes fall back without browser errors", async () => {
	const page = await open();
	const errors: string[] = [];
	const warnings: string[] = [];
	page.on("pageerror", (error) => errors.push(error.message));
	page.on("console", (message) => {
		if (message.type() === "warning") warnings.push(message.text());
	});
	try {
		const report = await page.evaluate(async () => {
			const watch = document.createElement("moq-watch");
			const publish = document.createElement("moq-publish");
			watch.setAttribute("visible", "always");
			watch.setAttribute("paused", "");
			document.body.append(watch, publish);
			for (const element of [watch, publish]) {
				element.setAttribute("url", "https://example.invalid/");
				element.setAttribute("url", "http://[");
			}
			watch.setAttribute("volume", "0.75");
			watch.setAttribute("volume", "foo");
			await Promise.resolve();
			const result = { watch: watch.url, publish: publish.url, volume: watch.volume };
			watch.remove();
			publish.remove();
			return result;
		});
		expect(report).toEqual({ watch: undefined, publish: undefined, volume: 0.5 });
		expect(errors).toEqual([]);
		expect(warnings.filter((message) => message.includes("invalid url="))).toHaveLength(2);
		expect(warnings.filter((message) => message.includes("invalid volume="))).toHaveLength(1);
	} finally {
		await page.close();
	}
});

test("captions load on selection and tear down across switches and close", async () => {
	const page = await open();
	await page.evaluate(() => window.startCaptions());
	const observers = await page.evaluate(() => window.activeObservers);
	try {
		expect(await page.locator("#captions").innerHTML()).toBe("");
		expect(await page.evaluate(() => window.activeObservers)).toBe(observers);
		expect(
			await page.evaluate(() =>
				performance.getEntriesByType("resource").some((entry) => entry.name.includes("/captions-")),
			),
		).toBe(false);
		await page.evaluate(() => window.captions.select("first"));
		await page.waitForFunction(() => document.querySelector("#captions")?.textContent?.includes("First caption"));
		expect(await page.locator("#captions style").count()).toBe(1);
		expect(await page.evaluate(() => window.activeObservers)).toBe(observers + 1);
		await page.evaluate(() => window.captions.select("second"));
		await page.waitForFunction(() => document.querySelector("#captions")?.textContent?.includes("Second caption"));
		expect(await page.locator("#captions").textContent()).not.toContain("First caption");
		expect(await page.locator("#captions style").count()).toBe(1);
		expect(await page.evaluate(() => window.activeObservers)).toBe(observers + 1);
		await page.evaluate(() => window.captions.select());
		await page.waitForFunction(() => document.querySelector("#captions")?.childElementCount === 0);
		expect(await page.evaluate(() => window.activeObservers)).toBe(observers);
		await page.evaluate(() => window.captions.select("first"));
		await page.waitForFunction(() => document.querySelector("#captions")?.textContent?.includes("First caption"));
		await page.evaluate(() => window.captions.close());
		await page.waitForFunction(() => document.querySelector("#captions")?.childElementCount === 0);
		expect(await page.evaluate(() => window.activeObservers)).toBe(observers);
	} finally {
		await page.close();
	}
});

test("closing while the caption chunk loads installs no stale overlay", async () => {
	const page = await open();
	await page.evaluate(() => window.startCaptions());
	const observers = await page.evaluate(() => window.activeObservers);
	let release!: () => void;
	const held = new Promise<void>((resolve) => {
		release = resolve;
	});
	try {
		await page.route("**/captions-*.js", async (route) => {
			await held;
			await route.continue();
		});
		const requested = page.waitForRequest((request) => request.url().includes("/captions-"));
		await page.evaluate(() => window.captions.select("first"));
		const request = await requested;
		await page.evaluate(() => window.captions.close());
		release();
		// Await the same module evaluation, then the queued renderer continuation.
		await page.evaluate(async (url) => {
			await import(url);
			await new Promise(requestAnimationFrame);
		}, request.url());
		expect(await page.locator("#captions").innerHTML()).toBe("");
		expect(await page.evaluate(() => window.activeObservers)).toBe(observers);
	} finally {
		release();
		await page.unrouteAll({ behavior: "wait" });
		await page.close();
	}
});
