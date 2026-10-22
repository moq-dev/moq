import { join } from "node:path";
import { parseArgs } from "node:util";
import type { Page } from "playwright";
import { check, finishTraces, launch, open, throwPageErrors } from "./harness";

const { values } = parseArgs({ options: { url: { type: "string" } } });
if (!values.url) throw new Error("vr.ts requires --url");
const run = process.env.MOQ_TEST_RUN;
if (!run) throw new Error("vr.ts runs under test/vr/run.sh, which builds the page into the run directory");

// Cross-origin isolated like the demo's vite server, so audio takes the SharedArrayBuffer path a deployment does.
const root = join(run, "vr-dist");
const server = Bun.serve({
	port: 0,
	async fetch(req) {
		let path = new URL(req.url).pathname;
		if (path === "/") path = "/index.html";
		const file = Bun.file(join(root, path));
		if (!(await file.exists())) return new Response("not found", { status: 404 });
		return new Response(file, {
			headers: { "Cross-Origin-Opener-Policy": "same-origin", "Cross-Origin-Embedder-Policy": "require-corp" },
		});
	},
});
const origin = `http://localhost:${server.port}`;

const VIEWPORT = { width: 640, height: 360 };
type Colour = "red" | "lime" | "blue" | "magenta" | "other";

// The colour at the centre of the page, read from a screenshot: WebGL discards its drawing buffer after
// each frame, so the canvas itself reads back blank.
async function centre(page: Page): Promise<{ colour: Colour; rgb: number[] }> {
	const png = (await page.screenshot()).toString("base64");
	const rgb = await page.evaluate(
		async ({ png, x, y }) => {
			const image = await createImageBitmap(await (await fetch(`data:image/png;base64,${png}`)).blob());
			const canvas = new OffscreenCanvas(image.width, image.height);
			const context = canvas.getContext("2d");
			if (!context) throw new Error("no 2d context");
			context.drawImage(image, 0, 0);
			return Array.from(context.getImageData(x, y, 1, 1).data.slice(0, 3));
		},
		{ png, x: VIEWPORT.width / 2, y: VIEWPORT.height / 2 },
	);
	const [r, g, b] = rgb;
	const hi = (v: number) => v > 150;
	const lo = (v: number) => v < 100;
	const colour: Colour =
		hi(r) && lo(g) && lo(b)
			? "red"
			: lo(r) && hi(g) && lo(b)
				? "lime"
				: lo(r) && lo(g) && hi(b)
					? "blue"
					: hi(r) && lo(g) && hi(b)
						? "magenta"
						: "other";
	return { colour, rgb };
}

async function waitForCentre(page: Page, want: Colour, timeout: number): Promise<void> {
	const deadline = Date.now() + timeout;
	let last = await centre(page);
	while (last.colour !== want && Date.now() < deadline) {
		await page.waitForTimeout(250);
		last = await centre(page);
	}
	check(last.colour === want, `centre is ${want}`, () => `got ${last.colour} rgb(${last.rgb.join(", ")})`);
}

// Render plane waves through the ambisonic decoder offline and return each ear's RMS level.
async function ears(page: Page, direction: number[], yaw: number): Promise<{ L: number; R: number }> {
	return page.evaluate(
		async ({ direction, yaw }) => {
			const { Spatial } = await import("/audio-check.js");
			const rate = 48_000;
			const context = new OfflineAudioContext(2, rate, rate);
			const buffer = context.createBuffer(4, rate, rate);
			const [x, y, z] = direction;
			// A 1 kHz plane wave encoded in ACN W, Y, Z, X with SN3D.
			[1, y, z, x].forEach((gain, channel) => {
				const data = buffer.getChannelData(channel);
				for (let i = 0; i < data.length; i++) data[i] = 0.5 * gain * Math.sin((2 * Math.PI * 1000 * i) / rate);
			});
			const discrete = {
				channelCount: 4,
				channelCountMode: "explicit",
				channelInterpretation: "discrete",
			} as const;
			const source = new AudioBufferSourceNode(context, { buffer, ...discrete });
			const root = new GainNode(context, discrete);
			source.connect(root);
			const spatial = new Spatial(root, { kind: "ambisonic" });
			// The head yawed left by `yaw` about +Y, standing at the origin.
			const head = {
				decompose: (
					position: { set(x: number, y: number, z: number): void },
					rotation: { set(x: number, y: number, z: number, w: number): void },
				) => {
					position.set(0, 0, 0);
					rotation.set(0, Math.sin(yaw / 2), 0, Math.cos(yaw / 2));
				},
			};
			spatial.pose(head);
			source.start();
			const rendered = await context.startRendering();
			// Skip the first 100 ms, while the listener glides into place.
			const rms = (channel: number) => {
				const data = rendered.getChannelData(channel).subarray(rate / 10);
				return Math.sqrt(data.reduce((sum, v) => sum + v * v, 0) / data.length);
			};
			return { L: rms(0), R: rms(1) };
		},
		{ direction, yaw },
	);
}

function louder(level: { L: number; R: number }, ear: "L" | "R" | "neither", assertion: string) {
	const ratio = level.L / level.R;
	const ok = ear === "L" ? ratio > 2 : ear === "R" ? ratio < 0.5 : ratio > 0.9 && ratio < 1.1;
	check(ok, assertion, () => `L ${level.L.toFixed(3)} R ${level.R.toFixed(3)}`);
}

const browser = await launch(["--use-angle=swiftshader", "--enable-unsafe-swiftshader"]);
let failed = true;
try {
	const query = new URLSearchParams({ url: values.url, name: "vr.hang" });
	const [page, errors] = await open(browser, `${origin}/?${query}`, "vr", true, { viewport: VIEWPORT });

	// Straight ahead is the left eye's centre: the catalog's stereo packing cropped the frame and the
	// equirectangular projection faced its centre forward.
	await waitForCentre(page, "red", 30_000);

	// Drag 90 degrees left (0.005 rad per pixel): a sphere wraps the left eye's left quarter there,
	// where a flat screen would show only the background.
	await page.mouse.move(VIEWPORT.width / 2, VIEWPORT.height / 2);
	await page.mouse.down();
	await page.mouse.move(VIEWPORT.width / 2 + Math.PI / 2 / 0.005, VIEWPORT.height / 2, { steps: 20 });
	await page.mouse.up();
	await waitForCentre(page, "lime", 5_000);

	louder(await ears(page, [0, 1, 0], 0), "L", "a source on the left is louder in the left ear");
	louder(await ears(page, [0, 1, 0], Math.PI / 2), "neither", "turning left to face that source centres it");
	louder(await ears(page, [1, 0, 0], Math.PI / 2), "R", "a source ahead moves to the right ear after turning left");

	throwPageErrors(errors);
	console.log("vr: stereo equirectangular painted per eye, facing forward; ambisonics follow the head");
	failed = false;
} finally {
	await finishTraces(failed);
	await browser.close();
	server.stop(true);
}
