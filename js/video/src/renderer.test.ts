import { afterEach, beforeEach, describe, expect, it } from "bun:test";
import { Signal } from "@moq/signals";
import { type Backend, Renderer } from "./renderer";

// Let the probe's promise chain and the effects it wakes run to completion.
async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
}

type Kind = "2d" | "webgpu";

// A canvas that, like a real one, keeps the first kind of context it hands out.
class FakeCanvas {
	width = 640;
	height = 360;
	kind?: Kind;
	draws: unknown[][] = [];
	transforms: number[][] = [];
	configured: GPUCanvasConfiguration[] = [];
	unconfigured = 0;

	getContext(kind: Kind): unknown {
		if (this.kind && this.kind !== kind) return null;
		this.kind = kind;
		if (kind === "2d") {
			return {
				canvas: this,
				fillStyle: "",
				save: () => {},
				restore: () => {},
				fillRect: () => {},
				setTransform: (...matrix: number[]) => this.transforms.push(matrix),
				drawImage: (...args: unknown[]) => this.draws.push(args),
			};
		}
		return {
			canvas: this,
			configure: (config: GPUCanvasConfiguration) => this.configured.push(config),
			unconfigure: () => this.unconfigured++,
			getCurrentTexture: () => ({ createView: () => ({}) }),
		};
	}
}

type DeviceOptions = { importThrows?: boolean; importInvalid?: boolean };

// Just enough of a GPUDevice for the probe and one pipeline.
class FakeDevice {
	destroyed = false;
	draws = 0;
	imports: unknown[] = [];
	options: DeviceOptions;
	#lose!: (info: { reason: string; message: string }) => void;
	lost = new Promise<{ reason: string; message: string }>((resolve) => {
		this.#lose = resolve;
	});
	#invalid?: { message: string };

	constructor(options: DeviceOptions = {}) {
		this.options = options;
	}

	lose() {
		this.#lose({ reason: "unknown", message: "test" });
	}

	destroy() {
		this.destroyed = true;
		this.#lose({ reason: "destroyed", message: "" });
	}

	pushErrorScope() {
		this.#invalid = undefined;
	}

	async popErrorScope() {
		return this.#invalid ?? null;
	}

	importExternalTexture(descriptor: { source: unknown }) {
		if (this.options.importThrows) throw new Error("import failed");
		if (this.options.importInvalid) this.#invalid = { message: "invalid import" };
		this.imports.push(descriptor.source);
		return {};
	}

	createShaderModule() {
		return {};
	}

	createRenderPipeline() {
		return { getBindGroupLayout: () => ({}) };
	}

	createSampler() {
		return {};
	}

	createBuffer() {
		return { destroy: () => {} };
	}

	createBindGroup() {
		return {};
	}

	createCommandEncoder() {
		return {
			beginRenderPass: () => ({
				setPipeline: () => {},
				setBindGroup: () => {},
				draw: () => this.draws++,
				end: () => {},
			}),
			finish: () => ({}),
		};
	}

	queue = { writeBuffer: () => {}, submit: () => {} };
}

type AdapterOptions = { fallback?: boolean; legacyFallback?: boolean; deviceThrows?: boolean; device?: DeviceOptions };

class Gpu {
	adapters: (AdapterOptions | null)[] = [];
	devices: FakeDevice[] = [];
	requests = 0;

	async requestAdapter() {
		this.requests++;
		const options = this.adapters.length > 1 ? this.adapters.shift() : this.adapters[0];
		if (!options) return null;
		return {
			info: options.legacyFallback ? undefined : { isFallbackAdapter: options.fallback ?? false },
			isFallbackAdapter: options.legacyFallback,
			requestDevice: async () => {
				if (options.deviceThrows) throw new Error("no device");
				const device = new FakeDevice(options.device);
				this.devices.push(device);
				return device;
			},
		};
	}

	getPreferredCanvasFormat() {
		return "bgra8unorm";
	}
}

function frame(timestamp = 1_000): VideoFrame {
	return {
		timestamp,
		colorSpace: { primaries: "bt709" },
		clone() {
			return this;
		},
		close() {},
	} as unknown as VideoFrame;
}

describe("Renderer", () => {
	let callbacks: Map<number, FrameRequestCallback>;
	let nextCallback: number;
	let gpu: Gpu;
	const saved = new Map<string, PropertyDescriptor | undefined>();

	function stub(target: object, key: string, value: unknown) {
		const id = `${target === globalThis ? "global" : "navigator"}.${key}`;
		if (!saved.has(id)) saved.set(id, Object.getOwnPropertyDescriptor(target, key));
		Object.defineProperty(target, key, { configurable: true, writable: true, value });
	}

	beforeEach(() => {
		callbacks = new Map();
		nextCallback = 0;
		gpu = new Gpu();

		stub(globalThis, "requestAnimationFrame", (callback: FrameRequestCallback) => {
			const id = ++nextCallback;
			callbacks.set(id, callback);
			return id;
		});
		stub(globalThis, "cancelAnimationFrame", (id: number) => callbacks.delete(id));
		stub(
			globalThis,
			"VideoFrame",
			class {
				close() {}
			},
		);
		stub(navigator, "gpu", gpu);
	});

	afterEach(() => {
		for (const [id, descriptor] of saved) {
			const [scope, key] = id.split(".");
			const target = scope === "global" ? globalThis : navigator;
			if (descriptor) Object.defineProperty(target, key, descriptor);
			else Reflect.deleteProperty(target, key);
		}
		saved.clear();
	});

	function paint(): number {
		const pending = [...callbacks.values()];
		callbacks.clear();
		for (const callback of pending) callback(0);
		return pending.length;
	}

	function create(canvas: FakeCanvas | Signal<FakeCanvas | undefined>, backend: Backend = "auto") {
		return new Renderer({
			canvas: canvas as unknown as Signal<HTMLCanvasElement | undefined>,
			frame: frame(),
			backend,
		});
	}

	it("picks WebGPU where it can import a VideoFrame", async () => {
		gpu.adapters = [{}];
		const canvas = new FakeCanvas();
		const renderer = create(canvas);
		try {
			await settle();
			expect(canvas.kind).toBe("webgpu");
			expect(paint()).toBe(1);
			expect(gpu.devices).toHaveLength(1);
			expect(gpu.devices[0].draws).toBe(1);
			expect(renderer.out.frame.peek()?.timestamp).toBe(1_000);
		} finally {
			renderer.close();
		}
		expect(gpu.devices[0].destroyed).toBe(true);
		expect(canvas.unconfigured).toBe(1);
	});

	const fallbacks: [string, () => void][] = [
		["navigator.gpu is missing", () => stub(navigator, "gpu", undefined)],
		["there is no adapter", () => (gpu.adapters = [null])],
		["the adapter is a software fallback", () => (gpu.adapters = [{ fallback: true }])],
		["the legacy adapter is a software fallback", () => (gpu.adapters = [{ legacyFallback: true }])],
		["requestDevice fails", () => (gpu.adapters = [{ deviceThrows: true }])],
		["the import throws", () => (gpu.adapters = [{ device: { importThrows: true } }])],
		["the import is invalid", () => (gpu.adapters = [{ device: { importInvalid: true } }])],
	];

	it.each(fallbacks)("falls back to Canvas2D when %s", async (_, setup) => {
		setup();
		const canvas = new FakeCanvas();
		const renderer = create(canvas);
		try {
			await settle();
			expect(canvas.kind).toBe("2d");
			expect(paint()).toBe(1);
			expect(canvas.draws).toHaveLength(1);
			expect(renderer.out.error.peek()).toBeUndefined();
			for (const device of gpu.devices) expect(device.destroyed).toBe(true);
		} finally {
			renderer.close();
		}
	});

	it.each(fallbacks)('refuses an explicit "webgpu" when %s', async (_, setup) => {
		setup();
		const canvas = new FakeCanvas();
		const renderer = create(canvas, "webgpu");
		try {
			await settle();
			expect(renderer.out.error.peek()).toBe("unsupported");
			expect(canvas.kind).toBeUndefined();
			expect(paint()).toBe(0);
		} finally {
			renderer.close();
		}
	});

	it('never probes WebGPU for "2d"', async () => {
		gpu.adapters = [{}];
		const canvas = new FakeCanvas();
		const renderer = create(canvas, "2d");
		try {
			await settle();
			expect(gpu.requests).toBe(0);
			expect(canvas.kind).toBe("2d");
			expect(paint()).toBe(1);
		} finally {
			renderer.close();
		}
	});

	it("replaces a lost device and keeps drawing", async () => {
		gpu.adapters = [{}];
		const canvas = new FakeCanvas();
		const renderer = create(canvas);
		try {
			await settle();
			expect(paint()).toBe(1);

			gpu.devices[0].lose();
			await settle();

			expect(gpu.devices).toHaveLength(2);
			expect(renderer.out.error.peek()).toBeUndefined();
			expect(paint()).toBe(1);
			expect(gpu.devices[1].draws).toBe(1);
			expect(canvas.configured.at(-1)?.device).toBe(gpu.devices[1] as unknown as GPUDevice);
		} finally {
			renderer.close();
		}
		expect(gpu.devices[1].destroyed).toBe(true);
	});

	it("reports surface-lost when no device replaces a lost one, and recovers on a fresh canvas", async () => {
		gpu.adapters = [{}];
		const canvas = new Signal<FakeCanvas | undefined>(new FakeCanvas());
		const renderer = create(canvas);
		try {
			await settle();
			expect(paint()).toBe(1);

			gpu.adapters = [null];
			gpu.devices[0].lose();
			await settle();

			expect(renderer.out.error.peek()).toBe("surface-lost");
			expect(paint()).toBe(0);

			// The lost canvas only hands out WebGPU, so recovery is a fresh canvas, which "auto"
			// draws to with Canvas2D while WebGPU is gone.
			const fresh = new FakeCanvas();
			canvas.set(fresh);
			await settle();

			expect(renderer.out.error.peek()).toBeUndefined();
			expect(fresh.kind).toBe("2d");
			expect(paint()).toBe(1);
			expect(fresh.draws).toHaveLength(1);
		} finally {
			renderer.close();
		}
	});

	it("reports surface-lost for a canvas that holds another kind of context", async () => {
		const canvas = new FakeCanvas();
		canvas.getContext("webgpu");
		const renderer = create(canvas, "2d");
		try {
			await settle();
			expect(renderer.out.error.peek()).toBe("surface-lost");
		} finally {
			renderer.close();
		}
	});

	it("repaints with the new presentation", async () => {
		const canvas = new FakeCanvas();
		const presentation = new Signal({ rotation: 0 });
		const renderer = new Renderer({
			canvas: canvas as unknown as HTMLCanvasElement,
			frame: frame(),
			presentation,
			backend: "2d",
		});
		try {
			await settle();
			expect(paint()).toBe(1);

			presentation.set({ rotation: 90 });
			await settle();

			expect(paint()).toBe(1);
			expect(canvas.transforms.at(-1)).toEqual([0, 1, -1, 0, 640, 0]);
			expect(canvas.draws.at(-1)?.slice(1)).toEqual([0, 0, 360, 640]);
		} finally {
			renderer.close();
		}
	});
});
