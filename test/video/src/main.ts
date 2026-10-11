/**
 * Page side of the renderer browser test: real WebGPU, a forced device loss, and the Canvas2D
 * fallback, driven by `renderer.browser.ts`.
 *
 * `gpu.ts` runs first: it lets the test lose the device and withhold the next adapter.
 *
 * @module
 */
import "./gpu.ts";
import "@moq/publish/element";
import { Signal } from "@moq/signals";
import * as Video from "@moq/video";

/** What a canvas shows: which context painted it, and the colour at each probed point. */
export type Snapshot = {
	context: "webgpu" | "2d" | "none";
	colors: string[];
};

/** The renderer check's observations, in order. */
export type RendererReport = {
	before: Snapshot;
	error: string | undefined;
	after: Snapshot;
	recovered: string | undefined;
};

/** The element check's observations, in order. */
export type ElementReport = {
	before: Snapshot;
	after: Snapshot;
};

declare global {
	interface Window {
		checkRenderer(): Promise<RendererReport>;
		checkElement(): Promise<ElementReport>;
	}
}

// Wait for `fn` to return something truthy, checking once per animation frame.
async function until<T>(what: string, fn: () => T | undefined | false): Promise<T> {
	for (let i = 0; i < 600; i++) {
		const value = fn();
		if (value) return value;
		await new Promise((resolve) => requestAnimationFrame(resolve));
	}
	throw new Error(`timed out waiting for ${what}`);
}

// Name the colour at a point coarsely, so the checks survive scaling and colour conversion.
function name([r, g, b]: Uint8ClampedArray): string {
	if (r > 160 && g < 96 && b < 96) return "red";
	if (b > 160 && r < 96 && g < 96) return "blue";
	if (r < 32 && g < 32 && b < 32) return "black";
	return "other";
}

function snapshot(canvas: HTMLCanvasElement, points: [number, number][]): Snapshot {
	const gpu = window.moqGpu.copy(canvas);
	const copy = new OffscreenCanvas(canvas.width, canvas.height);
	const ctx = copy.getContext("2d", { willReadFrequently: true });
	if (!ctx) throw new Error("no 2d context to read back with");
	ctx.drawImage(gpu ?? canvas, 0, 0);
	const colors = points.map(([x, y]) => name(ctx.getImageData(x, y, 1, 1).data));

	// Only a canvas that is not WebGPU-configured is read directly, so asking it for a 2d context
	// cannot create a WebGPU one. Ask only once something shows, since an unpainted canvas would
	// create a 2d context it then keeps.
	let context: Snapshot["context"] = "none";
	if (colors.some((color) => color !== "black")) {
		context = gpu ? "webgpu" : canvas.getContext("2d") ? "2d" : "none";
	}

	return { context, colors };
}

function painted(canvas: HTMLCanvasElement, points: [number, number][]): Snapshot | undefined {
	const shot = snapshot(canvas, points);
	return shot.context === "none" ? undefined : shot;
}

// A frame whose left half is red and right half is blue, so a rotation is visible in the output.
function halves(): VideoFrame {
	const source = new OffscreenCanvas(64, 32);
	const ctx = source.getContext("2d");
	if (!ctx) throw new Error("no 2d context to draw the frame with");
	ctx.fillStyle = "rgb(255, 0, 0)";
	ctx.fillRect(0, 0, 32, 32);
	ctx.fillStyle = "rgb(0, 0, 255)";
	ctx.fillRect(32, 0, 32, 32);
	return new VideoFrame(source, { timestamp: 0 });
}

// Rotated a quarter-turn clockwise, the left (red) half ends up on top of a 32x64 canvas.
const ROTATED: [number, number][] = [
	[16, 12],
	[16, 52],
];

window.checkRenderer = async () => {
	const frame = halves();
	const first = document.createElement("canvas");
	document.body.append(first);
	const canvas = new Signal<HTMLCanvasElement | undefined>(first);

	const renderer = new Video.Renderer({
		canvas,
		frame,
		display: { width: 32, height: 64 },
		presentation: { rotation: 90 },
	});

	try {
		const before = await until("the WebGPU paint", () => painted(first, ROTATED));

		window.moqGpu.lose();
		const error = await until("the surface-lost error", () => renderer.out.error.peek());

		const second = document.createElement("canvas");
		first.replaceWith(second);
		canvas.set(second);
		const after = await until("the Canvas2D paint", () => painted(second, ROTATED));

		return { before, error, after, recovered: renderer.out.error.peek() };
	} finally {
		renderer.close();
		frame.close();
	}
};

window.checkElement = async () => {
	const element = document.createElement("moq-publish");
	element.setAttribute("source", "camera");
	const first = document.createElement("canvas");
	element.append(first);
	document.body.append(element);

	// The fake camera's picture is mostly, but not entirely, non-black; any of a 3x3 grid will do.
	const grid = ({ width, height }: HTMLCanvasElement): [number, number][] =>
		[1, 2, 3].flatMap((i) => [1, 2, 3].map((j): [number, number] => [(width * i) / 4, (height * j) / 4]));

	try {
		const before = await until("the WebGPU preview", () => painted(first, grid(first)));

		window.moqGpu.lose();
		const second = await until("the swapped canvas", () => {
			const current = element.querySelector("canvas");
			return current && current !== first ? current : undefined;
		});

		const after = await until("the Canvas2D preview", () => painted(second, grid(second)));
		return { before, after };
	} finally {
		element.remove();
	}
};
