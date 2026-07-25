import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import type { Decoder } from "./decoder";
import { Renderer } from "./renderer";

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));

async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await flush();
}

describe("Renderer", () => {
	let callbacks: Map<number, FrameRequestCallback>;
	let nextCallback: number;
	let originalRequest: PropertyDescriptor | undefined;
	let originalCancel: PropertyDescriptor | undefined;

	beforeEach(() => {
		callbacks = new Map();
		nextCallback = 0;
		originalRequest = Object.getOwnPropertyDescriptor(globalThis, "requestAnimationFrame");
		originalCancel = Object.getOwnPropertyDescriptor(globalThis, "cancelAnimationFrame");

		Object.defineProperty(globalThis, "requestAnimationFrame", {
			configurable: true,
			value: (callback: FrameRequestCallback) => {
				const id = ++nextCallback;
				callbacks.set(id, callback);
				return id;
			},
		});
		Object.defineProperty(globalThis, "cancelAnimationFrame", {
			configurable: true,
			value: (id: number) => callbacks.delete(id),
		});
	});

	afterEach(() => {
		if (originalRequest) Object.defineProperty(globalThis, "requestAnimationFrame", originalRequest);
		else Reflect.deleteProperty(globalThis, "requestAnimationFrame");

		if (originalCancel) Object.defineProperty(globalThis, "cancelAnimationFrame", originalCancel);
		else Reflect.deleteProperty(globalThis, "cancelAnimationFrame");
	});

	function paint(): number {
		const pending = [...callbacks.values()];
		callbacks.clear();
		for (const callback of pending) callback(0);
		return pending.length;
	}

	it("repaints the current frame when presentation metadata changes", async () => {
		const transforms: number[][] = [];
		const draws: unknown[][] = [];
		let context: CanvasRenderingContext2D;
		const canvas = {
			width: 640,
			height: 360,
			getContext: () => context,
		} as unknown as HTMLCanvasElement;

		context = {
			canvas,
			fillStyle: "",
			save: () => {},
			restore: () => {},
			fillRect: () => {},
			scale: () => {},
			translate: () => {},
			setTransform: (...matrix: number[]) => transforms.push(matrix),
			drawImage: (...args: unknown[]) => draws.push(args),
		} as unknown as CanvasRenderingContext2D;

		const frame = {
			timestamp: 1_000,
			clone() {
				return this;
			},
			close() {},
		} as unknown as VideoFrame;
		const catalog = new Signal<Catalog.Video | undefined>({ renditions: {}, rotation: 0 });
		const decoder = {
			out: {
				display: new Signal({ width: 640, height: 360 }),
				frame: new Signal<VideoFrame | undefined>(frame),
			},
			source: { out: { catalog } },
		} as unknown as Decoder;
		const renderer = new Renderer(decoder, { canvas, visible: "never" });

		try {
			await settle();
			expect(paint()).toBe(1);
			expect(draws).toHaveLength(1);

			catalog.set({ renditions: {}, rotation: 90 });
			await settle();

			expect(paint()).toBe(1);
			expect(draws).toHaveLength(2);
			expect(transforms.at(-1)).toEqual([0, 1, -1, 0, 640, 0]);
			expect(draws.at(-1)?.slice(1)).toEqual([0, 0, 360, 640]);
		} finally {
			renderer.close();
		}
	});
});
