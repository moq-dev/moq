import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { Fanout } from "./fanout";
import { Renderer } from "./preview";

async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("Preview.Renderer", () => {
	let callbacks: FrameRequestCallback[];
	let originalRequest: PropertyDescriptor | undefined;
	let originalCancel: PropertyDescriptor | undefined;

	beforeEach(() => {
		callbacks = [];
		originalRequest = Object.getOwnPropertyDescriptor(globalThis, "requestAnimationFrame");
		originalCancel = Object.getOwnPropertyDescriptor(globalThis, "cancelAnimationFrame");
		Object.defineProperty(globalThis, "requestAnimationFrame", {
			configurable: true,
			value: (callback: FrameRequestCallback) => callbacks.push(callback),
		});
		Object.defineProperty(globalThis, "cancelAnimationFrame", {
			configurable: true,
			value: (id: number) => {
				callbacks[id - 1] = () => {};
			},
		});
	});

	afterEach(() => {
		if (originalRequest) Object.defineProperty(globalThis, "requestAnimationFrame", originalRequest);
		else Reflect.deleteProperty(globalThis, "requestAnimationFrame");
		if (originalCancel) Object.defineProperty(globalThis, "cancelAnimationFrame", originalCancel);
		else Reflect.deleteProperty(globalThis, "cancelAnimationFrame");
	});

	it("draws the captured frame mirrored at its own size through the shared renderer", async () => {
		const transforms: number[][] = [];
		const draws: unknown[][] = [];
		const canvas = { width: 300, height: 150 } as HTMLCanvasElement;
		Object.assign(canvas, {
			getContext: () => ({
				canvas,
				fillStyle: "",
				save: () => {},
				restore: () => {},
				fillRect: () => {},
				setTransform: (...matrix: number[]) => transforms.push(matrix),
				drawImage: (...args: unknown[]) => draws.push(args),
			}),
		});

		const frame = {
			timestamp: 0,
			displayWidth: 640,
			displayHeight: 360,
			clone() {
				return this;
			},
			close() {},
		} as unknown as VideoFrame;

		let push!: (frame: VideoFrame) => void;
		const fanout = new Fanout(
			new ReadableStream<VideoFrame>({
				start: (controller) => {
					push = (value) => controller.enqueue(value);
				},
			}),
		);

		const renderer = new Renderer({ canvas, frames: fanout, flip: true, backend: "2d" });
		try {
			push(frame);
			await settle();
			for (const callback of callbacks.splice(0)) callback(0);

			expect(canvas.width).toBe(640);
			expect(canvas.height).toBe(360);
			expect(transforms.at(-1)).toEqual([-1, 0, 0, 1, 640, 0]);
			expect(draws.at(-1)?.slice(1)).toEqual([0, 0, 640, 360]);
		} finally {
			renderer.close();
			fanout.close();
		}
	});
});
