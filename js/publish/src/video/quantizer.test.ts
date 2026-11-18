import { expect, test } from "bun:test";
import { Effect } from "@moq/signals";
import { type Codec, probeScale, Quantizer, type Scale } from "./quantizer";

const AVC: Scale = { codec: "avc", max: 51 };
const FPS = 30;
const PIXELS = 1280 * 720;

// An encoder whose frame is `complexity` bits at QP 0, halving every 6 QP, with a keyframe 2.5x a
// delta, about what Chrome's VP9 measured on high motion.
type Frame = { qp: number; key: boolean; bits: number; budget: number; timestamp: number };

function simulate(
	frames: number,
	scene: (i: number) => { complexity: number; budget: number; key?: boolean; drop?: boolean },
): Frame[] {
	const control = new Quantizer(AVC, PIXELS);
	const out: Frame[] = [];
	for (let i = 0; i < frames; i++) {
		const timestamp = Math.round((i * 1_000_000) / FPS);
		const { complexity, budget, key = i % 60 === 0, drop } = scene(i);
		const qp = control.next(timestamp, key, budget);
		const bits = Math.round(complexity * (key ? 2.5 : 1) * 2 ** (-qp / 6));
		if (drop) continue;
		control.output(timestamp, Math.ceil(bits / 8));
		out.push({ qp, key, bits, budget, timestamp });
	}
	return out;
}

// The longest a link at exactly the budget queues the output, in seconds.
function backlog(frames: Frame[], from = 0): number {
	let level = 0;
	let at = 0;
	let worst = 0;
	for (const frame of frames) {
		level = Math.max(0, level - (frame.budget * (frame.timestamp - at)) / 1_000_000) + frame.bits;
		at = frame.timestamp;
		if (frame.timestamp >= from) worst = Math.max(worst, level / frame.budget);
	}
	return worst;
}

function rate(frames: Frame[]): number {
	return (frames.reduce((sum, frame) => sum + frame.bits, 0) * FPS) / frames.length;
}

// Bits at QP 0 for a delta frame that costs `bits` at `qp`.
const at = (bits: number, qp: number) => bits * 2 ** (qp / 6);

test("a calm scene stays at the quality QP, well under the budget", () => {
	// 20 kbit deltas at QP 26: 600 kbps against a 4 Mbps budget.
	const frames = simulate(300, () => ({ complexity: at(20_000, 26), budget: 4_000_000 }));
	// Past the first keyframe, whose size is a guess until one is measured.
	expect(new Set(frames.slice(1).map((frame) => frame.qp))).toEqual(new Set([26]));
	expect(rate(frames)).toBeLessThan(1_000_000);
});

test("a busy scene spends the budget without overshooting it", () => {
	// Four times the budget at the quality QP.
	const frames = simulate(600, () => ({ complexity: at(133_333, 26), budget: 1_000_000 }));
	const busy = frames.slice(30);
	expect(rate(busy)).toBeGreaterThan(900_000);
	// No more than the budget, plus what the bucket may hold.
	const bits = busy.reduce((sum, frame) => sum + frame.bits, 0);
	expect(bits).toBeLessThanOrEqual(1_000_000 * (busy.length / FPS + 0.25));
	// The first keyframe's size is a guess; from then on nothing queues past the window.
	expect(backlog(frames, 1_000_000)).toBeLessThanOrEqual(0.25);
});

test("a scene cut overshoots by one frame, then settles under the window", () => {
	const frames = simulate(300, (i) => ({
		complexity: at(i < 150 ? 2_000 : 400_000, 26),
		budget: 2_000_000,
	}));
	const cut = frames[150];
	expect(cut.qp).toBe(26);
	// Only the cut itself, which no prediction saw coming, queues past the window.
	expect(backlog(frames)).toBeLessThanOrEqual(0.25 + cut.bits / cut.budget);
	expect(backlog(frames.slice(155), frames[155].timestamp)).toBeLessThanOrEqual(0.25);
	expect(frames[151].qp).toBeGreaterThan(26);
});

test("a smaller grant raises the QP on the next frame", () => {
	const frames = simulate(120, (i) => ({
		complexity: at(100_000, 26),
		budget: i < 60 ? 4_000_000 : 500_000,
		key: i === 0,
	}));
	expect(frames[59].qp).toBe(26);
	expect(frames[60].qp).toBeGreaterThan(30);
	expect(backlog(frames.slice(90), frames[90].timestamp)).toBeLessThanOrEqual(0.25);
});

test("quality returns a step per frame once a busy scene calms", () => {
	const frames = simulate(300, (i) => ({
		complexity: at(i < 150 ? 400_000 : 2_000, 26),
		budget: 1_000_000,
		key: i === 0,
	}));
	expect(frames[149].qp).toBeGreaterThan(40);
	for (let i = 151; i < 300; i++) expect(frames[i].qp).toBeGreaterThanOrEqual(frames[i - 1].qp - 1);
	expect(frames[299].qp).toBe(26);
});

test("a frame the encoder dropped doesn't hold the QP up", () => {
	const frames = simulate(120, (i) => ({
		complexity: at(i === 30 ? 4_000_000 : 2_000, 26),
		budget: 1_000_000,
		key: i === 0,
		drop: i === 30,
	}));
	expect(frames.at(-1)?.qp).toBe(26);
});

test("Chrome's VP9 qindex scale keeps the quality QP in its range", () => {
	const control = new Quantizer({ codec: "vp9", max: 255 }, PIXELS);
	expect(control.next(0, true, 100_000_000)).toBe(128);
	const spec = new Quantizer({ codec: "vp9", max: 63 }, PIXELS);
	expect(spec.next(0, true, 100_000_000)).toBe(32);
});

// Install an encoder whose keyframe is `size(quantizer)` bytes, failing like a strict browser on
// `"error"`, and probe `codec` with it.
async function probe(codec: Codec, size: (quantizer: number) => number | "error"): Promise<Scale | undefined> {
	class FakeEncoder {
		state: CodecState = "unconfigured";
		#init: VideoEncoderInit;
		#pending?: number | "error";
		constructor(init: VideoEncoderInit) {
			this.#init = init;
		}
		configure(config: VideoEncoderConfig): void {
			expect(config.bitrateMode).toBe("quantizer");
			this.state = "configured";
		}
		encode(_frame: unknown, options: Record<string, { quantizer: number }>): void {
			this.#pending = size(options[codec].quantizer);
		}
		async flush(): Promise<void> {
			if (this.#pending === "error") {
				this.state = "closed";
				throw new Error("quantizer out of range");
			}
			this.#init.output({ byteLength: this.#pending } as never, {} as never);
		}
		close(): void {
			this.state = "closed";
		}
	}
	class FakeFrame {
		close(): void {}
	}
	const encoder = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
	const frame = Object.getOwnPropertyDescriptor(globalThis, "VideoFrame");
	Object.defineProperty(globalThis, "VideoEncoder", { configurable: true, writable: true, value: FakeEncoder });
	Object.defineProperty(globalThis, "VideoFrame", { configurable: true, writable: true, value: FakeFrame });
	const effect = new Effect();
	try {
		const config = { codec: "", width: 1280, height: 720, bitrate: 1_000_000, bitrateMode: "constant" as const };
		const result = await probeScale(config, codec, effect);
		if (!result) throw new Error("probe superseded");
		return result.scale;
	} finally {
		effect.close();
		if (encoder) Object.defineProperty(globalThis, "VideoEncoder", encoder);
		else Reflect.deleteProperty(globalThis, "VideoEncoder");
		if (frame) Object.defineProperty(globalThis, "VideoFrame", frame);
		else Reflect.deleteProperty(globalThis, "VideoFrame");
	}
}

// A keyframe that halves every `halving` quantizer steps, up to `max`.
const halves = (halving: number, max: number) => (quantizer: number) =>
	Math.round(100_000 * 2 ** (-Math.min(quantizer, max) / halving));

test("Chrome's VP9 and AV1 read the quantizer as a qindex up to 255", async () => {
	expect(await probe("vp9", halves(30, 255))).toEqual({ codec: "vp9", max: 255 });
	expect(await probe("av1", halves(30, 255))).toEqual({ codec: "av1", max: 255 });
});

test("a VP9 encoder on the spec's scale keeps 63, whether it clamps or refuses past it", async () => {
	expect(await probe("vp9", halves(7.5, 63))).toEqual({ codec: "vp9", max: 63 });
	const strict = halves(7.5, 63);
	expect(await probe("vp9", (q) => (q > 63 ? "error" : strict(q)))).toEqual({ codec: "vp9", max: 63 });
});

test("H.264 honors 0..51", async () => {
	expect(await probe("avc", halves(6, 51))).toEqual({ codec: "avc", max: 51 });
});

test("an encoder that accepts quantizer mode but ignores the value has no scale", async () => {
	// Firefox: every quantizer encodes the same keyframe.
	expect(await probe("vp9", () => 31_137)).toBeUndefined();
	expect(await probe("avc", () => 31_137)).toBeUndefined();
});
