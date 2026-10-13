/**
 * Test hooks over the browser's real WebGPU, installed before anything requests a device.
 *
 * CI has no GPU, only SwiftShader, which reports itself as a fallback adapter that the renderer
 * rightly refuses. Report it as hardware so the WebGPU path runs; it imports and draws for real.
 *
 * A device cannot be lost on demand, so each device's `lost` promise is replaced with one the test
 * settles (a real loss still settles it). `lose()` also withholds every later adapter, which is the
 * case the renderer cannot recover from on the same canvas.
 *
 * @module
 */

declare global {
	interface Window {
		moqGpu: {
			/** Lose every device and withhold every later adapter. */
			lose(): void;
			/** What a WebGPU canvas showed after its latest submit, or undefined for any other canvas. */
			copy(canvas: HTMLCanvasElement): OffscreenCanvas | undefined;
		};
	}
}

Object.defineProperty(GPUAdapterInfo.prototype, "isFallbackAdapter", { get: () => false });

let withheld = false;
const losers: (() => void)[] = [];

const requestAdapter = GPU.prototype.requestAdapter;
GPU.prototype.requestAdapter = function (this: GPU, ...args: Parameters<GPU["requestAdapter"]>) {
	return withheld ? Promise.resolve(null) : requestAdapter.apply(this, args);
};

const requestDevice = GPUAdapter.prototype.requestDevice;
GPUAdapter.prototype.requestDevice = async function (
	this: GPUAdapter,
	...args: Parameters<GPUAdapter["requestDevice"]>
) {
	const device = await requestDevice.apply(this, args);
	const lost = Promise.withResolvers<GPUDeviceLostInfo>();
	void device.lost.then(lost.resolve);
	Object.defineProperty(device, "lost", { value: lost.promise });
	losers.push(() => lost.resolve({ reason: "unknown", message: "lost by the test" } as GPUDeviceLostInfo));
	return device;
};

// A WebGPU canvas reads back as transparent once its frame is presented, so copy every configured
// canvas right after each submit, while the drawn texture is still current.
const copies = new Map<HTMLCanvasElement, OffscreenCanvas>();

const configure = GPUCanvasContext.prototype.configure;
GPUCanvasContext.prototype.configure = function (this: GPUCanvasContext, config: GPUCanvasConfiguration) {
	if (this.canvas instanceof HTMLCanvasElement) copies.set(this.canvas, new OffscreenCanvas(1, 1));
	return configure.call(this, config);
};

const submit = GPUQueue.prototype.submit;
GPUQueue.prototype.submit = function (this: GPUQueue, buffers: Iterable<GPUCommandBuffer>) {
	submit.call(this, buffers);
	for (const [canvas, copy] of copies) {
		copy.width = canvas.width;
		copy.height = canvas.height;
		copy.getContext("2d")?.drawImage(canvas, 0, 0);
	}
};

window.moqGpu = {
	lose() {
		withheld = true;
		for (const lose of losers.splice(0)) lose();
	},
	copy(canvas) {
		return copies.get(canvas);
	},
};

export {};
