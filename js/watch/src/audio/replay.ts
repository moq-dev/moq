/**
 * Replay a recorded arrival trace through the player's real rings on a simulated clock.
 *
 * Every frame is inserted at the instant the trace says it reached the container consumer, and one
 * render quantum is read every quantum's worth of that same clock, which is what the AudioWorklet
 * does. {@link target} resolves the delay with a real {@link Sync} fed the recorded catalog config
 * and connection RTT, and the ring is sized from it the way the decoder sizes it, so a change to
 * either moves what a replay hears. Decoding is taken as instant and sample exact.
 *
 * Deterministic, so the audio quality harness grades its output with no headroom, and a unit test
 * can assert exact counts.
 *
 * @internal Test support, not part of the player.
 */
import type * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";
import { Signal } from "@moq/signals";
import { type Delay, Sync } from "../sync";
import { playbackJitter } from "./config";
import { ringSamples } from "./latency";
import { AudioRingBuffer } from "./ring-buffer";
import { allocSharedRingBuffer, SharedRingBuffer } from "./shared-ring-buffer";

/** An AudioWorklet render quantum, in frames. */
export const QUANTUM = 128;

/** A media gap longer than this is missing audio, not one long frame. */
const MAX_FRAME_MS = 100;

/** One frame reaching the container consumer. */
export interface Arrival {
	/** When it arrived, on the viewer's monotonic clock, in ms. */
	at: number;
	/** Its media timestamp, in ms. */
	timestamp: number;
}

/** What the player resolves its delay from. */
export interface Target {
	/** The configured delay, as the element takes it. */
	delay: Delay;
	/** The rendition as the catalog described it: its advertised jitter. */
	config: Catalog.AudioConfig;
	/** The connection's round trip, in ms, which "auto" sizes from. Unset is no PROBE. */
	rtt?: number;
}

/** The delay a player resolves for `input`, in ms, from a real {@link Sync}. */
export async function target(input: Target): Promise<Time.Milli> {
	const sync = new Sync({
		delay: input.delay,
		probe: input.rtt === undefined ? undefined : { rtt: Time.Milli(input.rtt) },
	});
	const unregister = sync.register(new Signal<Time.Milli | undefined>(playbackJitter(input.config)));
	try {
		// Effects flush on a microtask, and a timer runs after all of them.
		await new Promise((resolve) => setTimeout(resolve, 0));
		return sync.out.delay.peek();
	} finally {
		unregister();
		sync.close();
	}
}

/** What a replay plays through. */
export interface Options {
	/** `shared` is the SharedArrayBuffer ring a cross-origin isolated page runs; `post` is the rest. */
	ring: "shared" | "post";
	/** The sample rate, in Hz. */
	rate: number;
	/** The resolved delay the ring is sized to, in ms. See {@link target}. */
	delay: number;
}

/** One render quantum, as the worklet produced it. */
export interface Quantum {
	/** When it finished rendering, on the trace's clock, in ms. */
	at: number;
	/** The quantum: what the ring supplied at the front, zeros after it. Reused between yields. */
	output: Float32Array;
	/** Whether the ring was stalled after this quantum. */
	stalled: boolean;
	/** The playhead after this quantum, on the media clock, in ms. */
	timestamp: number;
}

/** The two rings behind the calls the decoder and the worklet make on them. */
interface Ring {
	insert(timestamp: Time.Micro, data: Float32Array[]): void;
	read(output: Float32Array[]): number;
	readonly stalled: boolean;
	readonly timestamp: Time.Micro;
}

/** The shared ring, sized the way `SharedAudioBuffer` sizes it. */
function shared(rate: number, latency: number): Ring {
	const ring = new SharedRingBuffer(allocSharedRingBuffer(1, Math.max(rate, latency * 2), rate));
	ring.setLatency(latency);
	return ring;
}

/** The postMessage ring, sized the way `PostAudioBuffer` asks the worklet to size it. */
function post(rate: number, latency: number): Ring {
	const ms = Time.Milli.fromSecond((latency / rate) as Time.Second);
	const ring = new AudioRingBuffer({ rate, channels: 1, latency: ms });
	return {
		insert: (timestamp, data) => ring.write(timestamp, data),
		read: (output) => ring.read(output),
		get stalled() {
			return ring.stalled;
		},
		get timestamp() {
			return ring.timestamp;
		},
	};
}

/**
 * Samples each frame carries: up to where the next one starts, so consecutive frames tile the
 * timeline exactly. A frame followed by a hole, or by nothing, gets the trace's typical frame.
 */
function frameSamples(trace: Arrival[], rate: number): number[] {
	const starts = [...new Set(trace.map((a) => a.timestamp))].sort((a, b) => a - b);
	const sample = (ms: number) => Math.round((ms * rate) / 1000);
	const spans = new Map<number, number>();
	for (let i = 0; i < starts.length - 1; i++) {
		if (starts[i + 1] - starts[i] < MAX_FRAME_MS) spans.set(starts[i], sample(starts[i + 1]) - sample(starts[i]));
	}
	const sorted = [...spans.values()].sort((a, b) => a - b);
	const typical = sorted[Math.floor(sorted.length / 2)];
	if (typical === undefined) throw new Error("a trace needs two frames less than 100 ms apart");
	return trace.map((a) => spans.get(a.timestamp) ?? typical);
}

/**
 * Play `trace` in simulated real time, yielding every render quantum from the first arrival to the
 * last.
 *
 * The audio is a constant, never zero, so what the ring did not supply is exactly the zeros at the
 * end of a quantum, which is how the harness's output tap reads a real page.
 */
export function* replay(trace: Arrival[], options: Options): Generator<Quantum> {
	const first = trace[0];
	const last = trace.at(-1);
	if (!first || !last) return;

	const { rate } = options;
	const samples = frameSamples(trace, rate);
	const ring = (options.ring === "shared" ? shared : post)(rate, ringSamples(rate, Time.Milli(options.delay)));

	const step = (QUANTUM / rate) * 1000;
	const output = new Float32Array(QUANTUM);
	let next = 0;

	for (let n = 1; first.at + (n - 1) * step <= last.at; n++) {
		// The quantum ending at `now` renders once everything that arrived by then is in.
		const now = first.at + n * step;
		while (next < trace.length && trace[next].at <= now) {
			const { timestamp } = trace[next];
			ring.insert(Time.Micro.fromMilli(timestamp as Time.Milli), [new Float32Array(samples[next]).fill(0.5)]);
			next++;
		}

		output.fill(0);
		ring.read([output]);
		yield { at: now, output, stalled: ring.stalled, timestamp: Time.Milli.fromMicro(ring.timestamp) };
	}
}
