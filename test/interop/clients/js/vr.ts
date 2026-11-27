import { join } from "node:path";
import { parseArgs } from "node:util";
import type { Page } from "playwright";
import { check, finishTraces, launch, open, throwPageErrors } from "./harness";

const { values } = parseArgs({ options: { url: { type: "string" } } });
if (!values.url) throw new Error("vr.ts requires --url");
const run = process.env.HARNESS_RUN_DIR;
if (!run) throw new Error("vr.ts runs under test/vr/run.sh, which builds the page into the run directory");

const root = join(run, "vr-dist");
const server = Bun.serve({
	port: 0,
	async fetch(req) {
		let path = new URL(req.url).pathname;
		if (path === "/") path = "/index.html";
		const file = Bun.file(join(root, path));
		if (!(await file.exists())) return new Response("not found", { status: 404 });
		return new Response(file);
	},
});
const origin = `http://localhost:${server.port}`;

const VIEWPORT = { width: 640, height: 360 };
// Radians the demo turns the camera per dragged pixel.
const DRAG = 0.005;
type Colour = "red" | "lime" | "blue" | "magenta" | "dark" | "other";

// What demo/vr puts on `window.vr`, as far as this reads it.
type Demo = {
	video: { out: { config: { peek(): unknown } } };
	audio: { out: { config: { peek(): unknown } } };
	scene: { listener?: { output: AudioNode } };
};

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
		r < 40 && g < 40 && b < 40
			? "dark"
			: hi(r) && lo(g) && lo(b)
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

// Turn the view left (right when negative) by dragging, as a desktop viewer looks around. The drag
// starts at the far edge so it stays on the page, where every pointer move lands.
async function turnLeft(page: Page, radians: number): Promise<void> {
	const { width, height } = VIEWPORT;
	const distance = radians / DRAG;
	check(Math.abs(distance) < width - 2, "the turn fits one drag", () => `${distance} px`);
	const start = distance > 0 ? 1 : width - 1;
	await page.mouse.move(start, height / 2);
	await page.mouse.down();
	await page.mouse.move(start + distance, height / 2, { steps: 20 });
	await page.mouse.up();
}

// Each ear's RMS level at the demo's spatial mix, averaged over half a second of live playback.
async function ears(page: Page): Promise<{ L: number; R: number }> {
	return page.evaluate(async () => {
		const { vr } = window as unknown as { vr: Demo };
		const output = vr.scene.listener?.output;
		if (!output) throw new Error("the demo has no spatial audio");
		const context = output.context;
		const split = new ChannelSplitterNode(context, { numberOfOutputs: 2 });
		output.connect(split);
		const meters = [0, 1].map((channel) => {
			const analyser = new AnalyserNode(context, { fftSize: 2048 });
			split.connect(analyser, channel);
			return analyser;
		});
		const sums = [0, 0];
		const data = new Float32Array(2048);
		for (let i = 0; i < 10; i++) {
			await new Promise((resolve) => setTimeout(resolve, 50));
			for (const [channel, meter] of meters.entries()) {
				meter.getFloatTimeDomainData(data);
				sums[channel] += data.reduce((sum, v) => sum + v * v, 0) / data.length;
			}
		}
		// The demo keeps its mix through catalog updates that move nothing, such as the publisher's
		// settling bitrate. A rebuilt mix would have dropped this tap and left the meters on silence.
		if (vr.scene.listener?.output !== output) throw new Error("the demo rebuilt its spatial mix mid-measurement");
		output.disconnect(split);
		const [L, R] = sums.map((sum) => Math.sqrt(sum / 10));
		return { L, R };
	});
}

function louder(level: { L: number; R: number }, ear: "L" | "neither", assertion: string) {
	const ratio = level.L / level.R;
	const ok = level.L + level.R > 0.01 && (ear === "L" ? ratio > 2 : ratio > 0.8 && ratio < 1.25);
	check(ok, assertion, () => `L ${level.L.toFixed(3)} R ${level.R.toFixed(3)}`);
}

const browser = await launch(["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"]);
let failed = true;
try {
	const query = new URLSearchParams({ url: values.url, name: "vr.hang" });
	const [page, errors] = await open(browser, `${origin}/?${query}`, "vr", true, { viewport: VIEWPORT });

	// The catalog the demo read carries what the importer found in the MP4's sv3d, st3d, and dOps.
	await page.waitForFunction(
		() => {
			const { vr } = window as unknown as { vr: Demo };
			return vr.video.out.config.peek() !== undefined && vr.audio.out.config.peek() !== undefined;
		},
		undefined,
		{ timeout: 30_000 },
	);
	const catalog = await page.evaluate(() => {
		const { vr } = window as unknown as { vr: Demo };
		const video = vr.video.out.config.peek() as { projection?: unknown; stereo?: unknown };
		const audio = vr.audio.out.config.peek() as { layout?: unknown; numberOfChannels?: unknown };
		return {
			projection: video.projection,
			stereo: video.stereo,
			layout: audio.layout,
			channels: audio.numberOfChannels,
		};
	});
	const want = {
		projection: { kind: "equirectangular", fov: 180 },
		stereo: "left-right",
		layout: "ambisonic",
		channels: 4,
	};
	check(Bun.deepEquals(catalog, want), "the catalog carries the fixture's VR fields", () => JSON.stringify(catalog));

	// Straight ahead is the left eye's centre: the stereo packing cropped the frame and the
	// equirectangular projection faced its centre forward.
	await waitForCentre(page, "red", 30_000);

	// The left eye's left quarter lies 45 degrees left over a 180 degree field.
	await turnLeft(page, Math.PI / 4);
	await waitForCentre(page, "lime", 5_000);

	// 120 degrees left is past the field's edge: empty scene. A full sphere would show the frame's blue.
	await turnLeft(page, (Math.PI * 75) / 180);
	await waitForCentre(page, "dark", 5_000);

	// Face forward again. The drags were gestures, so the audio graph is running. The fixture's sound
	// is a plane wave from the left: the left ear hears it, and turning to face it centres it.
	await turnLeft(page, (-Math.PI * 120) / 180);
	const ahead = await ears(page);
	louder(ahead, "L", "a source on the left is louder in the left ear");
	await turnLeft(page, Math.PI / 2);
	const facing = await ears(page);
	louder(facing, "neither", "turning left to face that source centres it");

	throwPageErrors(errors);
	const level = ({ L, R }: { L: number; R: number }) => `L ${L.toFixed(3)} R ${R.toFixed(3)}`;
	console.log(`vr: ${JSON.stringify(catalog)} laid out; ahead ${level(ahead)}, facing ${level(facing)}`);
	failed = false;
} finally {
	await finishTraces(failed);
	await browser.close();
	server.stop(true);
}
