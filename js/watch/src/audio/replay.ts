/**
 * Replay a recorded arrival trace through the player's consumer and rings on a simulated clock.
 *
 * Every frame is written into its group at the instant the trace says it arrived, and the player's
 * real `Container.Consumer` decides what to deliver, wait for, and skip, at the max age the delay
 * sets. Delivered frames go into the real ring, and a discontinuity resets it, the way the decoder
 * does. One render quantum is read every quantum's worth of that same clock, which is what the
 * AudioWorklet does. {@link target} resolves the delay with a real {@link Sync} fed the recorded
 * catalog config and connection RTT, and the ring is sized from it the way the decoder sizes it, so
 * a change to any of them moves what a replay hears. Decoding is taken as instant and sample exact.
 *
 * Deterministic, so the audio quality harness grades its output with no headroom, and a unit test
 * can assert exact counts.
 *
 * @internal Test support, not part of the player.
 */
import type * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import { Group, Time, Track } from "@moq/net";
import { Signal } from "@moq/signals";
import { type Delay, Sync } from "../sync";
import { playbackJitter } from "./config";
import { ringSamples } from "./latency";
import { AudioRingBuffer } from "./ring-buffer";
import { allocSharedRingBuffer, SharedRingBuffer } from "./shared-ring-buffer";
import { Terminal } from "./terminal";

/** An AudioWorklet render quantum, in frames. */
export const QUANTUM = 128;

/** A media gap longer than this is missing audio, not one long frame. */
const MAX_FRAME_MS = 100;

/**
 * The replay's own subscription passes every recorded group through: the recorder's relay already
 * applied a wire max age, so only the consumer's decisions are made again.
 */
const WIRE_MAX_AGE = Time.Milli(Number.MAX_SAFE_INTEGER);

/** Any non-empty payload: an empty one is the legacy container's end marker. */
const PAYLOAD = new Uint8Array(1);

/** One frame reaching the container consumer. */
export interface Arrival {
	/** When it arrived, on the viewer's monotonic clock, in ms. */
	at: number;
	/** Its media timestamp, in ms. */
	timestamp: number;
	/** The group that carried it. */
	group: number;
}

/** What the player resolves its delay from. */
export interface Target {
	/** The configured delay, as the element takes it. "instant" plays no audio, so it has no target. */
	delay: Exclude<Delay, "instant">;
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
	const jitter = playbackJitter(input.config);
	const unregister = sync.register(new Signal<Time.Milli | undefined>(jitter));
	try {
		// Sync's effects run on microtasks. The delay has settled once it holds this rendition's jitter
		// on top of the resolved jitter, the only rendition registered.
		while (sync.out.delay.peek() !== Time.Milli.add(sync.out.jitter.peek(), jitter)) {
			await sync.out.delay.changed();
		}
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
	/**
	 * The resolved delay, in ms. See {@link target}. It sizes the ring and is the consumer's max age,
	 * which `Sync` sets to the delay plus a lookahead a live viewer does not configure.
	 */
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
	reset(): void;
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
		reset: () => ring.reset(),
		get stalled() {
			return ring.stalled;
		},
		get timestamp() {
			return ring.timestamp;
		},
	};
}

/** A trace's media timestamp as the container carries it. */
const micros = (ms: number) => Math.round(ms * 1000) as Time.Micro;

/**
 * Samples each frame carries, by its timestamp: up to where the next one starts, so consecutive
 * frames tile the timeline exactly. A frame followed by a hole, or by nothing, gets the trace's
 * typical frame.
 */
function frameSamples(trace: Arrival[], rate: number): Map<Time.Micro, number> {
	const starts = [...new Set(trace.map((a) => a.timestamp))].sort((a, b) => a - b);
	const sample = (ms: number) => Math.round((ms * rate) / 1000);
	const spans = new Map<Time.Micro, number>();
	for (let i = 0; i < starts.length - 1; i++) {
		if (starts[i + 1] - starts[i] < MAX_FRAME_MS) {
			spans.set(micros(starts[i]), sample(starts[i + 1]) - sample(starts[i]));
		}
	}
	const sorted = [...spans.values()].sort((a, b) => a - b);
	const typical = sorted[Math.floor(sorted.length / 2)];
	if (typical === undefined) throw new Error("a trace needs two frames less than 100 ms apart");
	for (const start of starts) {
		if (!spans.has(micros(start))) spans.set(micros(start), typical);
	}
	return spans;
}

/**
 * Run every microtask already queued, and every one those queue, before returning.
 *
 * A macrotask starts only once the microtask queue is empty, and nothing between a group write and
 * the ring arms a timer, so this orders the replay behind the consumer rather than waiting on time.
 */
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

/**
 * Play `trace` in simulated real time, yielding every render quantum from the first arrival to the
 * last.
 *
 * A group's stream finishes with its last recorded frame. The audio is a constant, never zero, so
 * what the ring did not supply is exactly the zeros at the end of a quantum, which is how the
 * harness's output tap reads a real page.
 */
export async function* replay(trace: Arrival[], options: Options): AsyncGenerator<Quantum> {
	const first = trace[0];
	const last = trace.at(-1);
	if (!first || !last) return;

	const { rate } = options;
	const samples = frameSamples(trace, rate);
	const ring = (options.ring === "shared" ? shared : post)(rate, ringSamples(rate, Time.Milli(options.delay)));

	// Where each group's stream finishes.
	const ends = new Map<number, number>();
	trace.forEach((arrival, i) => {
		ends.set(arrival.group, i);
	});

	const track = new Track.Producer("audio");
	const consumer = new Container.Consumer(track.subscribe({ maxAge: WIRE_MAX_AGE }), {
		format: new Container.Legacy.Format("audio"),
		maxAge: Time.Milli(options.delay),
	});

	// The decoder's read loop, with the codec taken out. A failure is rethrown at the next quantum.
	const terminal = new Terminal();
	let failure: { error: unknown } | undefined;
	const decode = (async () => {
		for (;;) {
			const next = await consumer.next();
			if (!next) return;
			if (terminal.update(next)) ring.reset();
			if (next.end !== undefined || !next.frame) continue;

			const { timestamp } = next.frame;
			const length = samples.get(timestamp);
			if (length === undefined) throw new Error(`the consumer delivered an unrecorded frame at ${timestamp} us`);
			ring.insert(timestamp, [new Float32Array(length).fill(0.5)]);
		}
	})().catch((error: unknown) => {
		failure = { error };
	});

	const groups = new Map<number, Group.Producer>();
	const step = (QUANTUM / rate) * 1000;
	const output = new Float32Array(QUANTUM);
	let next = 0;

	try {
		for (let n = 1; first.at + (n - 1) * step <= last.at; n++) {
			// The quantum ending at `now` renders once everything that arrived by then is delivered.
			const now = first.at + n * step;
			const written = next;
			while (next < trace.length && trace[next].at <= now) {
				const arrival = trace[next];
				let group = groups.get(arrival.group);
				if (!group) {
					group = new Group.Producer(arrival.group);
					groups.set(arrival.group, group);
					track.writeGroup(group);
				}
				const timestamp = micros(arrival.timestamp);
				group.writeFrame({
					payload: Container.Legacy.encodeFrame(PAYLOAD, timestamp),
					timestamp: Time.Timestamp.fromMicros(timestamp),
				});
				if (ends.get(arrival.group) === next) {
					group.close();
					groups.delete(arrival.group);
				}
				next++;
			}
			if (next > written) await settle();
			if (failure) throw failure.error;

			output.fill(0);
			ring.read([output]);
			yield { at: now, output, stalled: ring.stalled, timestamp: Time.Milli.fromMicro(ring.timestamp) };
		}
	} finally {
		consumer.close();
		track.close();
		await decode;
	}
}
