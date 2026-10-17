/** CMAF fragment encode and decode over fixed audio and video fixtures, with no network. */
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import { check, payload } from "../fixture.ts";
import { longTasks, nextTask } from "./observe.ts";

/** One CMAF workload: a track kind, its frame sizes, and how often the whole set repeats. */
export interface Cmaf {
	kind: "audio" | "video";
	/** Bytes per frame; video's first frame is its keyframe. */
	sizes: number[];
	iterations: number;
}

/** The fragments and frames one phase handled, and its page-side cost. */
export interface Phase {
	fragments: number;
	bytes: number;
	digest: number;
	elapsed_ms: number;
	long_tasks: { count: number; total_ms: number };
}

const VIDEO = Catalog.VideoConfigSchema.parse({
	codec: "avc1.64001f",
	container: { kind: "legacy" },
	codedWidth: 1280,
	codedHeight: 720,
	description: "0164001fffe100046764001f01000268ee",
});
const AUDIO = Catalog.AudioConfigSchema.parse({
	codec: "opus",
	container: { kind: "legacy" },
	sampleRate: 48_000,
	numberOfChannels: 2,
});

let frames: Uint8Array[] = [];
let fragments: Uint8Array[] = [];
let init: Container.Cmaf.InitSegment | undefined;
let config: Cmaf | undefined;

/** Builds the fixture frames and init segment, outside any timed interval. */
function prepare(next: Cmaf): void {
	config = next;
	frames = next.sizes.map((size, frame) => payload(size, frame));
	fragments = [];
	init = Container.Cmaf.decodeInitSegment(
		next.kind === "video"
			? Container.Cmaf.createVideoInitSegment(VIDEO)
			: Container.Cmaf.createAudioInitSegment(AUDIO),
	);
}

// 30 fps video and 20 ms audio, in the init segment's timescale.
function duration(): number {
	if (!init || !config) throw new Error("cmaf before prepare");
	return config.kind === "video" ? init.timescale / 30 : init.timescale / 50;
}

async function encode(): Promise<Phase> {
	if (!config) throw new Error("encode before prepare");
	const step = duration();
	// The phase runs alone in its own task, so its long task is neither shared nor missed.
	await nextTask();
	const start = performance.now();
	fragments = [];
	let bytes = 0;
	for (let iteration = 0; iteration < config.iterations; iteration++) {
		for (const [index, data] of frames.entries()) {
			const sequence = iteration * frames.length + index;
			const fragment = Container.Cmaf.encodeDataSegment({
				kind: config.kind,
				data,
				timestamp: sequence * step,
				duration: step,
				keyframe: index === 0,
				sequence: sequence + 1,
			});
			bytes += fragment.byteLength;
			// Only the last iteration's fragments feed decode; earlier ones exist to be timed.
			if (iteration === config.iterations - 1) fragments.push(fragment);
		}
	}
	const end = performance.now();
	await nextTask();
	const segment = init;
	if (!segment) throw new Error("encode before prepare");
	// Checked after the interval: the encoder must carry every frame byte for byte.
	const digest = verify(fragments.map((fragment) => Container.Cmaf.decodeDataSegment(fragment, segment, undefined)));
	return {
		fragments: config.iterations * frames.length,
		bytes,
		digest,
		elapsed_ms: end - start,
		long_tasks: longTasks(start, end),
	};
}

async function decode(): Promise<Phase> {
	const segment = init;
	if (!config || !segment || fragments.length !== frames.length) throw new Error("decode before encode");
	await nextTask();
	const start = performance.now();
	let decoded: Container.Cmaf.Sample[][] = [];
	let bytes = 0;
	for (let iteration = 0; iteration < config.iterations; iteration++) {
		decoded = fragments.map((fragment) => {
			bytes += fragment.byteLength;
			return Container.Cmaf.decodeDataSegment(fragment, segment, undefined);
		});
	}
	const end = performance.now();
	await nextTask();
	return {
		fragments: config.iterations * fragments.length,
		bytes,
		digest: verify(decoded),
		elapsed_ms: end - start,
		long_tasks: longTasks(start, end),
	};
}

function verify(decoded: Container.Cmaf.Sample[][]): number {
	if (!config) throw new Error("verify before prepare");
	let digest = 0;
	for (const [frame, samples] of decoded.entries()) {
		if (samples.length !== 1) throw new Error(`fragment ${frame}: ${samples.length} samples`);
		if (config.kind === "video" && samples[0].keyframe !== (frame === 0))
			throw new Error(`fragment ${frame}: keyframe flag lost`);
		digest = (digest + check(samples[0].data, config.sizes[frame], frame, true)) >>> 0;
	}
	return digest;
}

declare global {
	interface Window {
		cmaf: { prepare: typeof prepare; encode: typeof encode; decode: typeof decode };
	}
}
window.cmaf = { prepare, encode, decode };
