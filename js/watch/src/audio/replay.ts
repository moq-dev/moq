/**
 * Replay a recorded arrival trace through the player's consumer and ring on a simulated clock.
 *
 * Every frame is written into its group at the instant the trace says it arrived, and the player's
 * real `Container.Consumer` decides what to deliver, wait for, and skip, at the max delay the delay
 * sets. Delivered frames go into the real ring, and a new codec epoch resets it, the way the decoder
 * does. One render quantum is read every quantum's worth of that same clock, which is what the
 * AudioWorklet does. A real {@link Sync} resolves the delay from the playout target the decoder
 * registers, measured by that same consumer from the recorded catalog config, and the ring and the
 * decode gate follow it the way the decoder's audio buffer moves them, so a change to any of them
 * moves what a replay hears. Decoding is taken as instant and sample exact.
 *
 * Deterministic, so the audio quality harness grades its output with no headroom, and a unit test
 * can assert exact counts.
 *
 * @internal Test support, not part of the player.
 */

import { Manual, type Micro, Milli, Nano, type Second } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import { Group, Time, Track } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { type Arrival as Diagnostic, gap } from "../arrivals";
import { type Delay, Sync } from "../sync";
import { Backpressure } from "./buffer";
import { frameDuration } from "./config";
import { target } from "./latency";
import { AudioRingBuffer } from "./ring-buffer";
import { Terminal } from "./terminal";

/** An AudioWorklet render quantum, in frames. */
export const QUANTUM = 128;

/**
 * The replay's own subscription passes every recorded group through: the recorder's relay already
 * applied a wire max delay, so only the consumer's decisions are made again.
 */
const WIRE_MAX_DELAY = Milli(Number.MAX_SAFE_INTEGER);

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
	/** The encoded frame's duration, in ms, measured before recording discards its payload. */
	duration: number;
}

/** What a replay plays through. */
export interface Options {
	/** Hold decoder delivery until this simulated monotonic time. */
	decodeAfter?: number;
	/** The sample rate, in Hz. */
	rate: number;
	/** The configured delay, as the element takes it. "instant" plays no audio, so it has no replay. */
	delay: Exclude<Delay, "instant">;
	/** The rendition as the catalog described it: its advertised jitter, delay, and codec. */
	config: Catalog.AudioConfig;
	/**
	 * How long the trace observed, from its first arrival, in ms. Rendering runs to here, so an
	 * outage after the last arrival is heard rather than cut off.
	 */
	duration: number;
}

/** One render quantum, as the worklet produced it. */
export interface Quantum {
	/** Every frame and gap the decoder received so far, in delivery order. Grows between yields. */
	arrivals: readonly Diagnostic[];
	/** When it finished rendering, on the trace's clock, in ms. */
	at: number;
	/** The quantum: what the ring supplied at the front, zeros after it. Reused between yields. */
	output: Float32Array;
	/** Whether the ring was stalled after this quantum. */
	stalled: boolean;
	/** The playhead after this quantum, on the media clock, in ms. */
	timestamp: number;
	/** The delay `Sync` resolved as of this quantum, in ms: what the ring and the decode gate hold. */
	delay: number;
}

/** Samples as the milliseconds `AudioBuffer` posts to the worklet. */
const millis = (rate: number, samples: number) => Milli.fromSecond((samples / rate) as Second);

/** A trace's media timestamp as the container carries it. */
const micros = (ms: number) => Math.round(ms * 1000) as Micro;

/**
 * Run every microtask already queued, and every one those queue, before returning.
 *
 * A macrotask starts only once the microtask queue is empty, and nothing between a group write and
 * the ring arms a timer, so this orders the replay behind the consumer rather than waiting on time.
 */
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

/**
 * Play `trace` in simulated real time, yielding every render quantum from the first arrival to the
 * end of the observation.
 *
 * A group's stream finishes with its last recorded frame. The audio is a constant, never zero, so
 * what the ring did not supply is exactly the zeros at the end of a quantum, which is how the
 * harness's output tap reads a real page.
 */
export async function* replay(trace: Arrival[], options: Options): AsyncGenerator<Quantum> {
	const first = trace[0];
	const last = trace.at(-1);
	if (!first || !last) return;
	const end = first.at + options.duration;
	if (end < last.at) throw new Error(`an arrival at ${last.at} ms is past the ${options.duration} ms observed`);

	const { rate, config } = options;
	const recorded = new Map<Micro, Arrival>();
	for (const arrival of trace) {
		if (!Number.isFinite(arrival.duration) || arrival.duration <= 0) {
			throw new Error(`invalid recorded frame duration: ${arrival.duration}`);
		}
		recorded.set(micros(arrival.timestamp), arrival);
	}

	// Where each group's stream finishes.
	const ends = new Map<number, number>();
	trace.forEach((arrival, i) => {
		ends.set(arrival.group, i);
	});

	const sync = new Sync({ delay: options.delay });
	const track = new Track.Producer("audio");
	const consumer = new Container.Consumer(track.subscribe({ maxDelay: WIRE_MAX_DELAY }), {
		format: new Container.Legacy.Format("audio"),
		maxDelay: sync.out.maxDelay,
	});

	// Match the decoder: a codec constant until a delivered packet states its own duration.
	const signals = new Effect();
	const frame = new Signal(frameDuration(config));
	const playout = signals.computed((effect) =>
		target({
			measured: effect.get(consumer.spread),
			advertised: config.jitter !== undefined ? Milli(config.jitter) : undefined,
			frame: effect.get(frame),
			delay: config.delay !== undefined ? Milli(config.delay) : undefined,
		}),
	);
	signals.cleanup(sync.register(playout));
	await settle();

	let delay = sync.out.delay.peek();
	// The decode gate, and the worklet's ring on the floor it sets, the way `AudioBuffer` drives both.
	const gate = new Backpressure(rate, false, delay);
	const ring = new AudioRingBuffer({ rate, channels: 1, latency: millis(rate, gate.floor) });
	const observe = () => gate.observe(ring.anchored ? ring.timestamp : undefined, ring.stalled);

	// Arrived but not yet decoded media, read or still in the consumer, is what covers the part of the
	// target held encoded, and what resumes a parked ring.
	const read = new Signal<Micro | undefined>(undefined);
	signals.run((effect) => {
		let newest = effect.get(read);
		for (const { end } of effect.get(consumer.buffered)) newest = Math.max(newest ?? 0, micros(end)) as Micro;
		if (gate.queued(newest)) ring.setLatency(millis(rate, gate.floor));
	});

	// The decoder's read loop, with the codec taken out. A failure is rethrown at the next quantum.
	const terminal = new Terminal();
	const arrivals: Diagnostic[] = [];
	const delivery = Promise.withResolvers<void>();
	if (options.decodeAfter === undefined) delivery.resolve();
	let failure: { error: unknown } | undefined;
	const abort = new AbortController();
	const decode = (async () => {
		await delivery.promise;
		for (;;) {
			const next = await consumer.next();
			if (!next) return;
			if (terminal.update(next)) {
				ring.reset();
				gate.reset();
			}
			if (next.kind === "gap") arrivals.push(gap(next));
			if (next.kind !== "frame") continue;

			const { timestamp, arrival } = next.frame;
			read.set(timestamp);
			const media = Milli.fromMicro(timestamp);
			const late = sync.received(media, arrival);
			arrivals.push({ kind: "frame", timestamp: media, arrival, late });
			const entry = recorded.get(timestamp);
			if (!entry) throw new Error(`the consumer delivered an unrecorded frame at ${timestamp} us`);
			frame.set(Milli(entry.duration));
			await gate.wait(timestamp, abort.signal);
			if (abort.signal.aborted) return;
			const pcm = new Float32Array(Math.round((entry.duration * rate) / 1000)).fill(0.5);
			ring.write(timestamp, [pcm]);
			// The decoder reports each output to the gate the way `AudioBuffer` and `Decoder.#emit` do.
			const end = (timestamp + micros((pcm.length / rate) * 1000)) as Micro;
			gate.inserted(timestamp, end);
			gate.decoded(end);
		}
	})().catch((error: unknown) => {
		failure = { error };
	});

	const groups = new Map<number, Group.Producer>();
	const step = (QUANTUM / rate) * 1000;
	const output = new Float32Array(QUANTUM);
	let next = 0;

	// The consumer stamps each arrival on the monotonic clock, which the estimator measures, so a
	// manual clock reads the trace's time instead until the replay finishes.
	const manual = new Manual();
	const moveTo = (ms: number) => manual.jump(Nano(Nano.fromMilli(Milli(ms)) - manual.now()));
	let clock = first.at;
	moveTo(clock);
	const restore = manual.install();

	try {
		for (let n = 1; first.at + (n - 1) * step <= end; n++) {
			// The quantum ending at `now` renders once everything that arrived by then is delivered.
			const now = first.at + n * step;
			let stamped = next;
			while (next < trace.length && trace[next].at <= now) {
				const arrival = trace[next];
				// The consumer stamps a frame as it reads it, so the clock moves on only once
				// everything that arrived at the previous instant has been read.
				if (arrival.at !== clock) {
					if (next > stamped) await settle();
					stamped = next;
					clock = arrival.at;
					moveTo(clock);
				}
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
			if (next > stamped) await settle();
			clock = now;
			moveTo(clock);
			if (options.decodeAfter !== undefined && now >= options.decodeAfter) {
				delivery.resolve();
				await settle();
			}
			if (failure) throw failure.error;

			// The decoder moves the ring and the gate as the resolved delay moves, which only an arrival
			// does.
			const resolved = sync.out.delay.peek();
			if (resolved !== delay) {
				delay = resolved;
				observe();
				const park = gate.setLatency(delay);
				ring.setLatency(millis(rate, gate.floor));
				if (park) ring.park();
			}

			output.fill(0);
			ring.read([output]);
			observe();
			yield {
				arrivals,
				at: now,
				output,
				stalled: ring.stalled,
				timestamp: Milli.fromMicro(ring.timestamp),
				delay,
			};
		}
	} finally {
		delivery.resolve();
		abort.abort();
		consumer.close();
		track.close();
		await decode;
		signals.close();
		sync.close();
		restore();
	}
}
