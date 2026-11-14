/**
 * Replay a recorded arrival trace through the player's consumer and ring on a simulated clock.
 *
 * Every frame is written into its group at the instant the trace says it arrived, and the player's
 * real `Container.Consumer` decides what to deliver, wait for, and skip, at the hole wait and max delay
 * the decoder sets. Delivered frames go into the real ring, which carries on through a gap the way the decoder's
 * does: a gap restarts only the codec. One render quantum is read every quantum's worth of that same
 * clock, which is what the AudioWorklet does. A real {@link Sync} resolves the delay from the playout target the decoder
 * registers, measured by that same consumer from the recorded catalog config, and the ring and the
 * decode gate follow it the way the decoder's audio buffer moves them, so a change to any of them
 * moves what a replay hears. Decoding is taken as instant and sample exact.
 *
 * Deterministic, so the audio quality harness grades its output with no headroom, and a unit test
 * can assert exact counts.
 *
 * @internal Test support, not part of the player.
 */

import { Duration, Instant } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import type * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import { type Group, Time, Track } from "@moq/net";
import { Derived, Effect, Signal } from "@moq/signals";
import { type Delay, STRETCH, Sync } from "../sync";
import { Backpressure } from "./buffer";
import { frameDuration } from "./config";
import { maxDelay as audioMaxDelay, bound, ringHold, target } from "./latency";
import { AudioRingBuffer } from "./ring-buffer";

/** An AudioWorklet render quantum, in frames. */
export const QUANTUM = 128;

/**
 * The replay's own subscription passes every recorded group through: the recorder's relay already
 * applied a wire max delay, so only the consumer's decisions are made again. No subscriber limit
 * leaves only the publisher's max age, which a replay never reaches.
 */
const WIRE_MAX_DELAY = null;

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
	/** The configured lookahead, in ms: nonzero plays in buffered mode, as the decoder does. */
	lookahead?: number;
	/** The configured stretch, in ms, as the element takes it (default: the element's). */
	stretch?: number;
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
	/** Frames the decoder received so far. */
	delivered: number;
	/** When it finished rendering, on the trace's clock, in ms. */
	at: number;
	/** The quantum: what the ring supplied at the front, zeros after it. Reused between yields. */
	output: Float32Array;
	/** Whether the ring was stalled after this quantum. */
	stalled: boolean;
	/** The playhead after this quantum, on the media clock, in ms. */
	timestamp: number;
	/** Where the shared clock puts video as of this quantum (`Sync.now()`), on the media clock, in ms. */
	clock?: number;
	/** The delay `Sync` resolved as of this quantum, in ms: what the ring and the decode gate hold. */
	delay: number;
	/** The ring's cumulative underruns, as the player's stats count them: each run-dry and each short quantum. */
	underruns: number;
	/** The ring's cumulative skips, as the player's stats count them: each jump that dropped queued audio. */
	skips: number;
	/** The received audio those skips dropped, cumulative, in ms. */
	trimmed: number;
	/** The received audio the stretch removed by playing faster, cumulative, in ms. */
	accelerated: number;
}

/**
 * A seeded speech-like signal, a pure function of media time so every replay hears the same audio.
 *
 * A constant matches itself at every shift, so the stretch's similarity search would show nothing;
 * speech's varying periods and pauses give it what a real talker does. Syllables of 120 to 300 ms glide the fundamental between 90 and 220 Hz under a
 * raised-cosine level, with pauses of 100 to 400 ms between some. The floor stays above the harness's
 * -60 dBFS silence classifier, and no sample is exactly zero, which the output tap reads as missing.
 */
export class Speech {
	readonly #rate: number;
	readonly #origin: number;
	#samples = new Float32Array(0);
	#length = 0;
	#phase = 0;
	#seed = 0x5eed;

	/** `origin` is the earliest media timestamp read, in ms. */
	constructor(rate: number, origin: number) {
		this.#rate = rate;
		this.#origin = origin;
	}

	/** `count` samples starting at media timestamp `timestamp`, in ms. */
	read(timestamp: number, count: number): Float32Array {
		const start = Math.round(((timestamp - this.#origin) * this.#rate) / 1000);
		if (start < 0) throw new Error(`speech read at ${timestamp} ms, before its ${this.#origin} ms origin`);
		while (this.#length < start + count) this.#syllable();
		return this.#samples.slice(start, start + count);
	}

	/** Mulberry32: uniform in [lo, hi). */
	#random(lo: number, hi: number): number {
		this.#seed = (this.#seed + 0x6d2b79f5) | 0;
		let t = this.#seed;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return lo + (((t ^ (t >>> 14)) >>> 0) / 4294967296) * (hi - lo);
	}

	#syllable() {
		const FLOOR = 0.03;
		const pause = this.#random(0, 1) < 0.25;
		const count = Math.round((this.#rate * (pause ? this.#random(100, 400) : this.#random(120, 300))) / 1000);
		const from = this.#random(90, 220);
		const to = this.#random(90, 220);
		const peak = pause ? FLOOR : this.#random(0.1, 0.5);
		if (this.#samples.length < this.#length + count) {
			const grown = new Float32Array(Math.max(2 * this.#samples.length, this.#length + count));
			grown.set(this.#samples.subarray(0, this.#length));
			this.#samples = grown;
		}
		for (let i = 0; i < count; i++) {
			const x = i / count;
			this.#phase += (2 * Math.PI * (from + (to - from) * x)) / this.#rate;
			const level = FLOOR + (peak - FLOOR) * 0.5 * (1 - Math.cos(2 * Math.PI * x));
			const p = this.#phase;
			const value = level * (Math.sin(p) + 0.5 * Math.sin(2 * p) + 0.25 * Math.sin(3 * p)) * 0.6;
			this.#samples[this.#length + i] = Math.abs(value) < 1e-6 ? 1e-6 : value;
		}
		this.#length += count;
	}
}

/** Samples as the milliseconds `AudioBuffer` posts to the worklet. */
const millis = (rate: number, samples: number) => (samples / rate) * 1000;

/** A trace's media timestamp in the whole microseconds the container carries. */
const micros = (ms: number) => Math.round(ms * 1000);

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
 * A group's stream finishes with its last recorded frame. The audio is {@link Speech}, never zero, so
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
	const recorded = new Map<number, Arrival>();
	for (const arrival of trace) {
		if (!Number.isFinite(arrival.duration) || arrival.duration <= 0) {
			throw new Error(`invalid recorded frame duration: ${arrival.duration}`);
		}
		recorded.set(micros(arrival.timestamp), arrival);
	}

	// Where each group's stream finishes.
	const ends = new Map<number, number>();
	const origin = trace.reduce((min, a) => Math.min(min, a.timestamp), Infinity);
	const speech = new Speech(rate, origin);
	trace.forEach((arrival, i) => {
		ends.set(arrival.group, i);
	});

	const lookahead = Duration.millis(options.lookahead ?? 0);
	const buffered = !lookahead.isZero();
	const stretch = options.stretch !== undefined ? Duration.millis(options.stretch) : STRETCH;
	const sync = new Sync({ delay: options.delay, lookahead, stretch });
	const track = new Track.Producer("audio");
	// The subscription's max delay, as the decoder sets it, which the consumer drops aged media at too.
	const maxDelay = new Derived([sync.out.delay, sync.out.maxDelay], (delay, max) =>
		audioMaxDelay({ delay, maxDelay: max }),
	);
	const consumer = new Container.Consumer(track.subscribe({ delay: { max: WIRE_MAX_DELAY } }), {
		format: new Container.Legacy.Format("audio"),
		delay: new Derived([sync.out.delay, maxDelay], (min, max) => ({ min, max })),
		wait: sync.out.maxDelay,
	});

	// Match the decoder: a codec constant until a delivered packet states its own duration.
	const signals = new Effect();
	const frame = new Signal(frameDuration(config));
	const playout = signals.computed((effect) =>
		target({
			measured: effect.get(consumer.spread),
			advertised: config.jitter !== undefined ? Duration.millis(config.jitter) : undefined,
			frame: effect.get(frame),
			delay: config.delay !== undefined ? Duration.millis(config.delay) : undefined,
		}),
	);
	// Buffered startup holds playback until the decoded span, plus whatever of the hold is queued
	// encoded past it, covers the delay, the way the decoder reports its startup span.
	const decoded = new Signal<{ start: Duration; end: Duration } | null>(null);
	const newest = new Signal<number | undefined>(undefined);
	const startup = buffered
		? signals.computed((effect) => {
				const span = effect.get(decoded);
				if (!span) return null;
				const queued = Duration.micros(effect.get(newest) ?? 0).saturatingSub(span.end);
				return { start: span.start, end: span.end.add(queued.min(ringHold(effect.get(sync.out.delay)))) };
			})
		: undefined;
	const audio = sync.track({ target: playout, startup });
	signals.cleanup(() => audio.close());
	await settle();

	let delay = sync.out.delay.peek();
	const terms = () => ({
		bound: bound({ delay, maxDelay: sync.out.maxDelay.peek(), stretch }).asMillis(),
	});
	// The decode gate, and the worklet's ring on the floor it sets, the way `AudioBuffer` drives both.
	const gate = new Backpressure(rate, buffered, delay);
	const ring = new AudioRingBuffer({ rate, channels: 1, latency: millis(rate, gate.floor), ...terms() });
	const observe = () => gate.observe(ring.anchored ? ring.timestamp : undefined, ring.stalled);

	// Arrived but not yet decoded media, read or still in the consumer, is what covers the part of the
	// target held encoded before playback starts.
	// In microseconds, as the gate takes them.
	const read = new Signal<number | undefined>(undefined);
	// What the consumer holds has arrived too, so its newest end is how far the received audio reaches.
	signals.run((effect) => {
		let queued = effect.get(read);
		let arrived: number | undefined;
		for (const { end } of effect.get(consumer.buffered)) {
			const micro = Math.round(end.asMicros());
			queued = Math.max(queued ?? 0, micro);
			arrived = Math.max(arrived ?? 0, micro);
		}
		newest.set(queued);
		gate.queued(queued);
		if (arrived !== undefined) ring.received(arrived);
	});

	// The decoder's read loop, with the codec taken out. Traces record no declared ends, so a gap is
	// the only codec epoch a replay meets. A failure is rethrown at the next quantum.
	let delivered = 0;
	const delivery = Promise.withResolvers<void>();
	if (options.decodeAfter === undefined) delivery.resolve();
	let failure: { error: unknown } | undefined;
	const abort = new AbortController();
	const decode = (async () => {
		await delivery.promise;
		for (;;) {
			const next = await consumer.next();
			if (!next) return;
			if (next.kind !== "frame") continue;
			delivered++;

			const { timestamp: media, arrival } = next.frame;
			const timestamp = media.asMicros();
			read.set(timestamp);
			audio.received(media, arrival);
			const entry = recorded.get(timestamp);
			if (!entry) throw new Error(`the consumer delivered an unrecorded frame at ${timestamp} us`);
			frame.set(Duration.millis(entry.duration));
			// The decoder tells the ring what arrived before it waits to decode it.
			ring.received(timestamp + micros(entry.duration));
			await gate.wait(timestamp, abort.signal);
			if (abort.signal.aborted) return;
			const pcm = speech.read(entry.timestamp, Math.round((entry.duration * rate) / 1000));
			ring.write(timestamp, [pcm]);
			// The decoder reports each output to the gate the way `AudioBuffer` and `Decoder.#emit` do.
			const end = timestamp + micros((pcm.length / rate) * 1000);
			gate.inserted(timestamp, end);
			decoded.update((span) => ({ start: span?.start ?? media, end: Duration.micros(end) }));
			gate.decoded(end, end);
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
	const moveTo = (ms: number) => manual.jump(Duration.millis(ms).saturatingSub(manual.now().since(Instant.ZERO)));
	let clock = first.at;
	moveTo(clock);
	const installed = manual.install();

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
					group = track.createGroup(BigInt(arrival.group));
					groups.set(arrival.group, group);
				}
				const timestamp = micros(arrival.timestamp);
				group.writeFrame({
					payload: Container.Legacy.encodeFrame(PAYLOAD, Duration.micros(timestamp)),
					timestamp: Time.Timestamp.fromMicros(timestamp),
				});
				if (ends.get(arrival.group) === next) {
					group.finish();
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
			if (!resolved.eq(delay)) {
				delay = resolved;
				observe();
				gate.setLatency(delay);
				ring.setLatency(millis(rate, gate.floor), terms());
			}

			// The worklet reads nothing until startup releases it, then the shared clock as of the
			// quantum's start, mapped onto its own time.
			const shared = sync.now();
			ring.clock(shared !== undefined ? (shared.asMillis() - step) * 1000 : undefined);
			output.fill(0);
			if (sync.playing.peek()) ring.read([output]);
			observe();
			yield {
				delivered,
				at: now,
				output,
				stalled: ring.stalled,
				timestamp: ring.timestamp / 1000,
				clock: sync.now()?.asMillis(),
				delay: delay.asMillis(),
				underruns: ring.underruns,
				skips: ring.skips,
				trimmed: ring.trimmed,
				accelerated: ring.accelerated,
			};
		}
	} finally {
		delivery.resolve();
		abort.abort();
		consumer.close();
		track.finish();
		track.close();
		await decode;
		signals.close();
		sync.close();
		installed.close();
	}
}
