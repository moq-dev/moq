import { type Presentation, texturePresentationTransform } from "./presentation";

// One oversized triangle covers the viewport, so there is no vertex buffer and no diagonal seam.
// The vertex stage maps each canvas coordinate to the frame coordinate it shows, which handles
// rotation and flip; the interpolation is exact because the map is affine.
const SHADER = /* wgsl */ `
struct Params {
	x: vec4<f32>,
	y: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var frame: texture_external;

struct Vertex {
	@builtin(position) position: vec4<f32>,
	@location(0) uv: vec2<f32>,
}

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> Vertex {
	let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
	let p = vec3<f32>(uv, 1.0);
	var out: Vertex;
	out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
	out.uv = vec2<f32>(dot(params.x.xyz, p), dot(params.y.xyz, p));
	return out;
}

@fragment
fn fragment(in: Vertex) -> @location(0) vec4<f32> {
	return vec4<f32>(textureSampleBaseClampToEdge(frame, samp, in.uv).rgb, 1.0);
}
`;

// GPUBufferUsage is a global only where WebGPU exists, so spell out the bits.
const UNIFORM = 0x40;
const COPY_DST = 0x08;

// The deprecated spelling Chrome shipped before GPUAdapterInfo carried it.
type LegacyAdapter = GPUAdapter & { isFallbackAdapter?: boolean };

/**
 * Check WebGPU can render a `VideoFrame`, and return the device it used.
 *
 * Resolves with a reason string when any check fails, so `"auto"` can log why it fell back.
 */
export async function probe(): Promise<GPUDevice | string> {
	if (!navigator.gpu) return "navigator.gpu is missing";

	const adapter = await navigator.gpu.requestAdapter();
	if (!adapter) return "no adapter";
	if (adapter.info?.isFallbackAdapter ?? (adapter as LegacyAdapter).isFallbackAdapter) {
		return "the adapter is a software fallback";
	}

	let device: GPUDevice;
	try {
		device = await adapter.requestDevice();
	} catch (err) {
		return `requestDevice failed: ${err}`;
	}

	// An invalid import returns a texture instead of throwing, so catch it with an error scope too.
	const frame = new VideoFrame(new Uint8Array(4), { format: "RGBA", codedWidth: 1, codedHeight: 1, timestamp: 0 });
	device.pushErrorScope("validation");
	let thrown: unknown;
	try {
		device.importExternalTexture({ source: frame });
	} catch (err) {
		thrown = err;
	} finally {
		frame.close();
	}
	const invalid = await device.popErrorScope();

	if (thrown !== undefined || invalid) {
		device.destroy();
		return `importExternalTexture(VideoFrame) failed: ${thrown ?? invalid?.message}`;
	}

	return device;
}

/** Draws frames into a configured WebGPU canvas context with one device. */
export class Gpu {
	#context: GPUCanvasContext;
	#device: GPUDevice;
	#format: GPUTextureFormat;
	#colorSpace?: PredefinedColorSpace;

	#pipeline: GPURenderPipeline;
	#sampler: GPUSampler;
	#params: GPUBuffer;

	constructor(context: GPUCanvasContext, device: GPUDevice) {
		this.#context = context;
		this.#device = device;
		this.#format = navigator.gpu.getPreferredCanvasFormat();

		const module = device.createShaderModule({ code: SHADER });
		this.#pipeline = device.createRenderPipeline({
			layout: "auto",
			vertex: { module, entryPoint: "vertex" },
			fragment: { module, entryPoint: "fragment", targets: [{ format: this.#format }] },
			primitive: { topology: "triangle-list" },
		});
		this.#sampler = device.createSampler({ magFilter: "linear", minFilter: "linear" });
		this.#params = device.createBuffer({ size: 32, usage: UNIFORM | COPY_DST });

		this.#configure("srgb");
	}

	#configure(colorSpace: PredefinedColorSpace) {
		if (this.#colorSpace === colorSpace) return;
		this.#colorSpace = colorSpace;
		this.#context.configure({ device: this.#device, format: this.#format, colorSpace, alphaMode: "opaque" });
	}

	/**
	 * Draw `frame`, or clear to black without one.
	 *
	 * Import, bind, encode, and submit happen with no `await` in between: the external texture
	 * expires once the frame is closed or the task ends.
	 */
	draw(frame: VideoFrame | undefined, presentation: Presentation | undefined) {
		// Render in the frame's own gamut so a Display P3 source is not clipped to sRGB. The DOM
		// typings list only the SDR primaries, so widen to compare against "smpte432" (Display P3).
		if (frame) {
			const primaries: string | null | undefined = frame.colorSpace?.primaries;
			this.#configure(primaries === "smpte432" ? "display-p3" : "srgb");
		}

		const encoder = this.#device.createCommandEncoder();
		const pass = encoder.beginRenderPass({
			colorAttachments: [
				{
					view: this.#context.getCurrentTexture().createView(),
					clearValue: { r: 0, g: 0, b: 0, a: 1 },
					loadOp: "clear",
					storeOp: "store",
				},
			],
		});

		if (frame) {
			const canvas = this.#context.canvas;
			const { x, y } = texturePresentationTransform({ width: canvas.width, height: canvas.height }, presentation);
			this.#device.queue.writeBuffer(this.#params, 0, new Float32Array([...x, 0, ...y, 0]));

			const texture = this.#device.importExternalTexture({ source: frame, colorSpace: this.#colorSpace });
			const group = this.#device.createBindGroup({
				layout: this.#pipeline.getBindGroupLayout(0),
				entries: [
					{ binding: 0, resource: { buffer: this.#params } },
					{ binding: 1, resource: this.#sampler },
					{ binding: 2, resource: texture },
				],
			});

			pass.setPipeline(this.#pipeline);
			pass.setBindGroup(0, group);
			pass.draw(3);
		}

		pass.end();
		this.#device.queue.submit([encoder.finish()]);
	}

	/** Release the GPU resources this surface created. The caller owns the device. */
	close() {
		this.#params.destroy();
		this.#context.unconfigure();
	}
}
