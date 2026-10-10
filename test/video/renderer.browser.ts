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
