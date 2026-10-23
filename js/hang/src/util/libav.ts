let loading: Promise<boolean> | undefined;

// The WebCodecs registry's AAC codec strings. The polyfill refuses every "mp4a" string, so these
// reach libav's own AAC decoder through the polyfill's libav.js codec escape hatch instead.
const AAC = new Set(["mp4a.40.2", "mp4a.40.02", "mp4a.40.5", "mp4a.40.05", "mp4a.40.29", "mp4a.67"]);

/** Load the libav WebCodecs polyfill (Opus, plus AAC decoding) if AudioEncoder/AudioDecoder are missing. Resolves true once available. */
export async function polyfill(): Promise<boolean> {
	if (globalThis.AudioEncoder && globalThis.AudioDecoder) {
		return true;
	}

	if (!loading) {
		console.warn("using libav WebCodecs polyfill; performance may be degraded");

		// @moq/libav is libav.js's opus-af variant plus the AAC decoder, which no published variant has.
		// I forked libavjs-webcodecs-polyfill to avoid Typescript errors; there's no changes otherwise.
		loading = Promise.all([import("@moq/libav"), import("@kixelated/libavjs-webcodecs-polyfill")]).then(
			async ([{ default: LibAV }, libav]) => {
				const native = globalThis.AudioDecoder;
				await libav.load({ LibAV, polyfill: true });
				fixPlanarCopy(libav.AudioData as unknown as typeof AudioData);
				if (native) return true;

				type Config = Parameters<InstanceType<typeof libav.AudioDecoder>["configure"]>[0];
				const aac = (config: Config) => typeof config.codec === "string" && AAC.has(config.codec);
				const route = (config: Config): Config =>
					aac(config) ? { ...config, codec: { libavjs: { codec: "aac" } } } : config;

				class AudioDecoder extends libav.AudioDecoder {
					#planar = false;

					constructor(init: ConstructorParameters<typeof libav.AudioDecoder>[0]) {
						super({ ...init, output: (data) => init.output(this.#planar ? planar(data) : data) });
					}

					override configure(config: Config): void {
						this.#planar = aac(config);
						super.configure(route(config));
					}

					static override async isConfigSupported(config: Config) {
						const { supported } = await libav.AudioDecoder.isConfigSupported(route(config));
						return { supported, config };
					}
				}
				globalThis.AudioDecoder = AudioDecoder as unknown as typeof globalThis.AudioDecoder;
				return true;
			},
		);
	}
	return await loading;
}

// libav's AAC decoder always emits planar float, but the polyfill concatenates the planes and labels
// the result interleaved "f32". Relabel it so channels read back from the right plane.
function planar<T extends AudioData>(data: T): T {
	if (data.format !== "f32") return data;
	const samples = new Float32Array(data.numberOfFrames * data.numberOfChannels);
	data.copyTo(samples, { planeIndex: 0 });
	const relabeled = new (data.constructor as typeof AudioData)({
		format: "f32-planar",
		sampleRate: data.sampleRate,
		numberOfFrames: data.numberOfFrames,
		numberOfChannels: data.numberOfChannels,
		timestamp: data.timestamp,
		data: samples,
	});
	data.close();
	return relabeled as T;
}

// Each interleaved sample format, with the offset and scale that map it to [-1, 1].
const INTERLEAVED = {
	u8: [Uint8Array, 0x80, 0x80],
	s16: [Int16Array, 0, 0x8000],
	s32: [Int32Array, 0, 0x80000000],
	f32: [Float32Array, 0, 1],
} as const;

// The polyfill converts interleaved samples to "f32-planar" from the wrong offset, so every channel
// after the first reads past its frames (NaN) instead of its own samples. libopus decodes
// interleaved, so stereo Opus lost its right channel. Deinterleave from a same-format copy instead,
// which the polyfill gets right.
function fixPlanarCopy(Data: typeof AudioData): void {
	const copyTo = Data.prototype.copyTo;
	Data.prototype.copyTo = function (this: AudioData, destination: AllowSharedBufferSource, options) {
		const format = this.format ?? "";
		if (options.format !== "f32-planar" || !(format in INTERLEAVED)) return copyTo.call(this, destination, options);
		const [View, offset, scale] = INTERLEAVED[format as keyof typeof INTERLEAVED];
		const channels = this.numberOfChannels;
		const index = (value: number) => Number.isSafeInteger(value) && value >= 0;
		if (!index(options.planeIndex) || options.planeIndex >= channels) {
			throw new RangeError(`no plane ${options.planeIndex}`);
		}

		const start = options.frameOffset ?? 0;
		const count = options.frameCount ?? this.numberOfFrames - start;
		if (!index(start) || !index(count) || start >= this.numberOfFrames || start + count > this.numberOfFrames) {
			throw new RangeError("frames out of range");
		}
		// Bounded by the caller's view, not the buffer behind it.
		if (count * Float32Array.BYTES_PER_ELEMENT > destination.byteLength) {
			throw new RangeError("destination too small");
		}
		const samples = new View(this.numberOfFrames * channels);
		copyTo.call(this, samples, { planeIndex: 0 });
		const out = new Float32Array(count);
		for (let i = 0; i < count; i++)
			out[i] = (samples[(start + i) * channels + options.planeIndex] - offset) / scale;
		// Copied as bytes: a destination need not be 4-byte aligned.
		const bytes = ArrayBuffer.isView(destination)
			? new Uint8Array(destination.buffer, destination.byteOffset, out.byteLength)
			: new Uint8Array(destination, 0, out.byteLength);
		bytes.set(new Uint8Array(out.buffer));
	};
}
