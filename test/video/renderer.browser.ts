/**
 * The `@moq/video` renderer in headless Chromium: paint through WebGPU, lose the device with no
 * adapter to replace it, check the renderer reports an `InvalidStateError`, then check a fresh canvas
 * paints through Canvas2D. Once against the renderer directly and once through `<moq-publish>`,
 * which swaps its own canvas.
 *
 * Not `*.test.ts`: `just test video` launches the Chromium provided by the root devShell.
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
		expect(report.error).toBe("InvalidStateError");
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

for (const audio of [false, true]) {
	test(`buffered ${audio ? "A/V" : "video"} paints a poster before the common floor`, async () => {
		const page = await open();
		try {
			await page.evaluate((audio) => window.startPoster(audio, true), audio);
			if (audio) {
				await page.locator("body").click();
				await page.waitForFunction(() => window.poster.state().audioState === "running");
			}
			await page.evaluate(() => {
				window.poster.push(0);
				window.poster.push(40_000);
			});
			await page.waitForFunction(() => window.poster.state().color === "red");
			// Browser paint observation spans several floor intervals, so a time-only gate fails.
			await page.waitForTimeout(300);
			expect(await page.evaluate(() => window.poster.state())).toMatchObject({
				timestamp: 0,
				color: "red",
				playing: false,
			});
			await page.evaluate(() => window.poster.push(100_000));
			if (audio) {
				await page.waitForTimeout(300);
				expect(await page.evaluate(() => window.poster.state())).toMatchObject({
					timestamp: 0,
					color: "red",
					playing: false,
				});
				await page.evaluate(() => window.poster.audio());
			}
			await page.waitForFunction(() => window.poster.state().color === "blue");
			expect(await page.evaluate(() => window.poster.state().playing)).toBe(true);
		} finally {
			await page.evaluate(() => window.poster?.close());
			await page.close();
		}
	}, 30_000);
}

test("live video does not wait to fill the buffered startup floor", async () => {
	const page = await open();
	try {
		await page.evaluate(() => window.startPoster(false, false));
		await page.evaluate(() => {
			window.poster.push(0);
			window.poster.push(40_000);
		});
		await page.waitForFunction(() => window.poster.state().color === "blue");
		expect(await page.evaluate(() => window.poster.state().timestamp)).toBe(40_000);
	} finally {
		await page.evaluate(() => window.poster?.close());
		await page.close();
	}
}, 30_000);

test("buffered audio keeps its opening while video fills, with a running audio context", async () => {
	const page = await open();
	try {
		await page.evaluate(() => window.startPoster(true, true));
		await page.locator("body").click();
		await page.waitForFunction(() => window.poster.state().audioState === "running");
		await page.evaluate(() => window.poster.audio());
		const initial = await page.evaluate(() => window.poster.state().audioTimestamp);
		await page.waitForTimeout(300);
		expect(await page.evaluate(() => window.poster.state())).toMatchObject({
			audioTimestamp: initial,
			playing: false,
		});
		await page.evaluate(() => {
			window.poster.push(0);
			window.poster.push(40_000);
		});
		await page.waitForFunction(() => window.poster.state().color === "red");
		expect(await page.evaluate(() => window.poster.state().playing)).toBe(false);
		await page.evaluate(() => window.poster.push(100_000));
		await page.waitForFunction(
			() => window.poster.state().color === "blue" && (window.poster.state().audioTimestamp ?? 0) > 0,
		);
	} finally {
		await page.evaluate(() => window.poster?.close());
		await page.close();
	}
}, 30_000);

test("disconnect releases capture, preview, audio context, and the watch GPU, and reconnect resumes", async () => {
	const page = await open();
	try {
		await page.evaluate(() => window.startLifecycle());
		await page.locator("body").click();
		await page.waitForFunction(() => {
			const state = window.lifecycle.state();
			return state.tracks.length === 2 && state.context && state.devices === 1;
		});
		await page.evaluate(() => window.lifecycle.move());
		expect(await page.evaluate(() => window.lifecycle.state())).toMatchObject({
			tracks: ["live", "live"],
			devices: 1,
		});
		await page.evaluate(() => window.lifecycle.detach());
		await page.waitForFunction(() => {
			const state = window.lifecycle.state();
			return (
				state.tracks.every((track) => track === "ended") &&
				state.context === "closed" &&
				state.devices === 0 &&
				!state.preview &&
				state.retained
			);
		});
		await page.evaluate(() => window.lifecycle.reconnect());
		await page.waitForFunction(() => {
			const state = window.lifecycle.state();
			return (
				state.tracks.length === 2 &&
				state.tracks.every((track) => track === "live") &&
				state.devices === 1 &&
				state.retained
			);
		});
	} finally {
		await page.evaluate(() => window.lifecycle?.close());
		await page.close();
	}
}, 30_000);

test("a paused decoder holds its picture until its broadcast is disabled", async () => {
	const page = await open();
	try {
		await page.evaluate(() => window.startPoster(false, false));
		await page.evaluate(() => window.poster.push(0));
		await page.waitForFunction(() => window.poster.state().decoded);
		await page.evaluate(() => window.poster.paused(true));
		await page.waitForFunction(() => !window.poster.state().downloading);
		expect(await page.evaluate(() => window.poster.state().decoded)).toBe(true);
		await page.evaluate(() => window.poster.enabled(false));
		await page.waitForFunction(
			() => !window.poster.state().decoded && window.poster.state().timestamp === undefined,
		);
	} finally {
		await page.evaluate(() => window.poster?.close());
		await page.close();
	}
}, 30_000);
