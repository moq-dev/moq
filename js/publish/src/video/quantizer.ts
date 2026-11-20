import type { Effect } from "@moq/signals";

/** The WebCodecs encode option that carries a codec's per-frame quantizer. */
export type Codec = "avc" | "hevc" | "vp9" | "av1";

/** How the encoder reads a per-frame quantizer, as the probe measured it. */
export type Scale = {
	codec: Codec;
	// The largest quantizer the encoder honors: 51 for H.264 and HEVC; 63 for VP9 and AV1 as their
	// registrations specify, or 255 where the browser reads it as a qindex, as Chrome does.
	max: number;
};

// The encode option key for `codec`, or undefined when its registration has no per-frame quantizer.
export function codecKey(codec: string): Codec | undefined {
	if (codec.startsWith("avc1")) return "avc";
	if (codec.startsWith("hev1")) return "hevc";
	if (codec.startsWith("vp09")) return "vp9";
	if (codec.startsWith("av01")) return "av1";
	return undefined;
}

// Seconds of the budget the bucket holds: the most a link at exactly the budget queues, short of a
// scene cut, which no prediction sees coming. Measured in Chrome on 720p30 VP9 and AV1, 0.25 s keeps
// a high-motion clip at 1.10x its cap over any second (0.5 s: 1.24x) for half a QP of quality.
const WINDOW = 0.25;

// The bucket level a run of delta frames settles at, leaving the rest for keyframes.
const TARGET = 0.5;

// How full a keyframe may leave the bucket.
const KEY_FILL = 0.75;

// How many frames pay the bucket back to its target. Measured: 8 halves the QP swing of 2.
const CATCHUP = 8;

// The most QP falls per frame, in halvings, so quality steps back up instead of pulsing.
const DROP = 1 / 6;

// How far a smaller delta frame moves the size estimate, while a larger one replaces it at once.
// Encoders alternate frame sizes by reference structure; following each one swung QP 3x as much.
const SMOOTH = 0.1;

// The first keyframe's assumed bits per pixel at the quality QP, before any frame is measured.
const BPP = 1;

// The least a keyframe costs over a delta frame at the same QP. The last keyframe can be from another
// scene, so a scene that got busier since predicts its next keyframe from its delta frames instead.
// Measured in Chrome: about 2.6x on high motion, and far more on calm scenes, which the last keyframe covers.
const KEY_RATIO = 2;

// A frame inside the encoder: its timestamp, QP, and predicted size in bits.
type Pending = { timestamp: number; qp: number; bits: number };

/**
 * Per-frame QP for WebCodecs' quantizer mode: a constant quality QP, raised only as far as keeps the
 * output under a hard peak at the budget.
 *
 * The output fills a leaky bucket that drains at the budget and holds {@link WINDOW} of it. Each
 * frame gets the quality QP unless its predicted size would fill the bucket past its target, and then
 * the lowest QP whose prediction fits. A frame's size is known only once the encoder outputs it, so
 * the frames still inside count at their prediction.
 */
export class Quantizer {
	readonly #max: number;
	// The quality QP: what a frame gets while the budget allows.
	readonly #base: number;
	// The QP increase that halves a frame.
	readonly #halving: number;

	// Bits in the bucket, as of `#at` microseconds.
	#level = 0;
	#at?: number;
	// The microseconds between the last two encoded frames, and the last one's timestamp.
	#interval = 1_000_000 / 30;
	#encoded?: number;
	// The last delta frame's QP, which the next falls from by at most DROP.
	#qp: number;
	#pending: Pending[] = [];
	// Each frame type's size in bits at QP 0, extrapolated from its outputs.
	#key: number;
	#delta?: number;

	constructor(scale: Scale, pixels: number) {
		this.#max = scale.max;
		if (scale.codec === "avc" || scale.codec === "hevc") {
			this.#base = 26;
			this.#halving = 6;
		} else {
			// Measured in Chrome on qindex 0..255: about 30 per halving. The spec's 0..63 is a quarter.
			const unit = scale.max / 255;
			this.#base = Math.round(128 * unit);
			this.#halving = 30 * unit;
		}
		this.#qp = this.#base;
		this.#key = pixels * BPP * 2 ** (this.#base / this.#halving);
	}

	/**
	 * Whether to encode the frame at `timestamp` (microseconds) under a budget of `bitrate` bits per
	 * second: not while the bucket is full, as when the scene outgrows even the largest QP or the
	 * grant is zero, so the backlog never passes the window by more than a frame.
	 */
	admit(timestamp: number, bitrate: number): boolean {
		this.#drain(timestamp, bitrate);
		return this.#level < bitrate * WINDOW;
	}

	/** The QP for an admitted frame at `timestamp`, under a budget of `bitrate` bits per second. */
	next(timestamp: number, key: boolean, bitrate: number): number {
		this.#drain(timestamp, bitrate);
		// Between encoded frames, not every frame offered: a capped frame rate or a held frame drops some.
		if (this.#encoded !== undefined && timestamp > this.#encoded) this.#interval = timestamp - this.#encoded;
		this.#encoded = timestamp;

		const capacity = bitrate * WINDOW;
		const share = (bitrate * this.#interval) / 1_000_000;
		const allowance = key
			? share + Math.max(0, capacity * KEY_FILL - this.#level)
			: Math.max(share / 8, share + (capacity * TARGET - this.#level) / CATCHUP);

		const complexity = key ? Math.max(this.#key, (this.#delta ?? 0) * KEY_RATIO) : (this.#delta ?? this.#key);
		let qp = Math.max(Math.ceil(this.#halving * Math.log2(complexity / allowance)), this.#base);
		if (!key) qp = Math.max(qp, this.#qp - Math.ceil(this.#halving * DROP));
		qp = Math.min(qp, this.#max);
		if (!key) this.#qp = qp;

		const bits = complexity * 2 ** (-qp / this.#halving);
		this.#pending.push({ timestamp, qp, bits });
		this.#level += bits;
		return qp;
	}

	/** Account the encoded frame at `timestamp`, `bytes` long, a keyframe if `key`. */
	output(timestamp: number, bytes: number, key: boolean): void {
		// A frame the encoder dropped never outputs; forget its prediction.
		while (this.#pending.length > 0 && this.#pending[0].timestamp < timestamp) {
			const dropped = this.#pending.shift();
			if (dropped) this.#level = Math.max(0, this.#level - dropped.bits);
		}
		const frame = this.#pending[0]?.timestamp === timestamp ? this.#pending.shift() : undefined;
		const bits = bytes * 8;
		this.#level = Math.max(0, this.#level + bits - (frame?.bits ?? 0));
		if (!frame) return;

		// By what the encoder made, which may be a keyframe nobody asked for.
		const complexity = bits * 2 ** (frame.qp / this.#halving);
		if (key) this.#key = complexity;
		else if (this.#delta === undefined || complexity > this.#delta) this.#delta = complexity;
		else this.#delta += (complexity - this.#delta) * SMOOTH;
	}

	// Empty the bucket at `bitrate` up to `timestamp`.
	#drain(timestamp: number, bitrate: number): void {
		if (this.#at !== undefined && timestamp > this.#at) {
			this.#level = Math.max(0, this.#level - (bitrate * (timestamp - this.#at)) / 1_000_000);
		}
		this.#at = timestamp;
	}
}

// The largest picture the scale probe encodes.
const PROBE = { width: 320, height: 240 };

// How much smaller a frame at a higher quantizer must come out to count as honoring it.
const HONORED = 2;

/**
 * Measure how `config`'s encoder reads a per-frame quantizer: whether it honors one at all, and
 * whether VP9 and AV1 read it as a qindex up to 255. No scale when it ignores the quantizer, as
 * Firefox does while still reporting the mode supported; undefined when superseded. Encodes one
 * textured keyframe per quantizer, each flushed on its own, since a value past the spec's range may
 * fail the encoder.
 */
export async function probeScale(
	config: VideoEncoderConfig,
	codec: Codec,
	effect: Effect,
): Promise<{ scale?: Scale } | undefined> {
	const spec = codec === "avc" || codec === "hevc" ? 51 : 63;
	const values = codec === "avc" || codec === "hevc" ? [spec >> 2, spec] : [spec >> 2, spec, 255];

	let size = 0;
	const encoder = new VideoEncoder({
		output: (chunk) => {
			size += chunk.byteLength;
		},
		// flush() rejects with the same error.
		error: () => {},
	});
	// A superseded probe must release its codec even if the browser never settles flush().
	const close = () => {
		if (encoder.state !== "closed") encoder.close();
	};
	effect.cleanup(close);

	// Small, since the verdict doesn't depend on the size and a detailed keyframe is slow to encode.
	const width = Math.min(config.width, PROBE.width);
	const height = Math.min(config.height, PROBE.height);
	const frame = texture(width, height);
	const sizes: number[] = [];
	try {
		encoder.configure({ ...config, width, height, bitrateMode: "quantizer" });
		for (const quantizer of values) {
			size = 0;
			const options: EncodeOptions = { keyFrame: true, [codec]: { quantizer } };
			encoder.encode(frame, options);
			const flushed = await effect.race(
				encoder.flush().then(
					() => true,
					() => false,
				),
			);
			if (flushed === undefined) return;
			if (!flushed) break;
			sizes.push(size);
		}
	} finally {
		frame.close();
		close();
	}

	const [low, high, extended] = sizes;
	if (high === undefined || !(low > Math.min(high, extended ?? high) * HONORED)) return {};
	const max = extended !== undefined && high > extended * HONORED ? 255 : spec;
	return { scale: { codec, max } };
}

/** Encode options with every codec's per-frame quantizer, which TypeScript's DOM lib lists only for AVC. */
export type EncodeOptions = VideoEncoderEncodeOptions & { [codec in Codec]?: { quantizer: number } };

// A deterministic picture with detail at every scale, so its size tracks the quantizer.
function texture(width: number, height: number): VideoFrame {
	const luma = width * height;
	const data = new Uint8Array((luma * 3) / 2).fill(0x80);
	let seed = 1;
	for (let y = 0; y < height; y++) {
		for (let x = 0; x < width; x++) {
			seed = (seed * 1103515245 + 12345) >>> 0;
			const wave = 48 * Math.sin(x / 7 + 3 * Math.sin(y / 11)) * Math.cos(y / 5);
			data[y * width + x] = 128 + wave + ((seed >>> 27) - 16);
		}
	}
	return new VideoFrame(data, { format: "I420", codedWidth: width, codedHeight: height, timestamp: 0 });
}
