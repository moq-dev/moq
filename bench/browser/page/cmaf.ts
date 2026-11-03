/** CMAF fragment decode over fixed audio and video input, with no network in the timed phase. */
import * as CmafDecode from "@moq/hang/cmaf";
import { check } from "../fixture.ts";
import { longTasks, nextTask } from "./observe.ts";

/** One CMAF workload: a track kind, its frame sizes, and how often the whole set repeats. */
export interface Cmaf {
	kind: "audio" | "video";
	/** Bytes per frame; video's first frame is its keyframe. */
	sizes: number[];
	iterations: number;
}

/** The fragments and bytes one phase handled, and its page-side cost. */
export interface Phase {
	fragments: number;
	bytes: number;
	elapsed_ms: number;
	long_tasks: { count: number; total_ms: number };
}

let fragments: Uint8Array[] = [];
let decoded: CmafDecode.Sample[][] | undefined;
let init: CmafDecode.InitSegment | undefined;
let config: Cmaf | undefined;

/** Load fixed fragment input outside the timed interval. */
async function prepare(next: Cmaf): Promise<void> {
	const response = await fetch("/cmaf-fixtures.json");
	if (!response.ok) throw new Error(`CMAF fixture: ${response.status}`);
	const fixtures = await response.json();
	const bytes = (value: string) => Uint8Array.from(atob(value), (char) => char.charCodeAt(0));
	config = next;
	init = CmafDecode.decodeInitSegment(bytes(next.kind === "video" ? fixtures.videoInit : fixtures.audioInit));
	fragments = next.sizes.map((_, frame) =>
		bytes(
			next.kind === "audio"
				? fixtures.bench.audio
				: frame === 0
					? fixtures.bench.videoKey
					: fixtures.bench.videoDelta,
		),
	);
	decoded = undefined;
}

async function decode(): Promise<Phase> {
	const segment = init;
	if (!config || !segment || fragments.length !== config.sizes.length) throw new Error("decode before prepare");
	await nextTask();
	const start = performance.now();
	let bytes = 0;
	for (let iteration = 0; iteration < config.iterations; iteration++) {
		decoded = fragments.map((fragment) => {
			bytes += fragment.byteLength;
			return CmafDecode.decodeDataSegment(fragment, segment, undefined);
		});
	}
	const end = performance.now();
	await nextTask();
	return {
		fragments: config.iterations * fragments.length,
		bytes,
		elapsed_ms: end - start,
		long_tasks: longTasks(start, end),
	};
}

/** Check the last decode's output byte for byte outside its measured phase. */
function verify(): number {
	if (!config || !decoded) throw new Error("verify before decode");
	let digest = 0;
	for (const [frame, sample] of decoded.entries()) {
		if (sample.length !== 1) throw new Error(`fragment ${frame}: ${sample.length} samples`);
		if (config.kind === "video" && sample[0].keyframe !== (frame === 0))
			throw new Error(`fragment ${frame}: keyframe flag lost`);
		const pattern = config.kind === "video" && frame > 0 ? 1 : 0;
		digest = (digest + check(sample[0].data, config.sizes[frame], pattern, true)) >>> 0;
	}
	return digest;
}

declare global {
	interface Window {
		cmaf: { prepare: typeof prepare; decode: typeof decode; verify: typeof verify };
	}
}
window.cmaf = { prepare, decode, verify };
