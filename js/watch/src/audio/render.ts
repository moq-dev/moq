import type { Micro, Milli } from "@moq/clock";

/** Everything the main thread sends the render worklet over its port. */
export type Message = Init | Data | Latency | Reset | Truncate | Playing | End | Clock;

/** Create the worklet's ring. */
export interface Init {
	type: "init";
	channels: number;
	rate: number;
	latency: Milli;
	// Buffered mode: anchor to the first frame and play through. The ring holds the floor; the
	// lookahead above it is held back upstream (the main thread applies the backpressure).
	buffered: boolean;
	playing?: boolean;
}

/** Flush the buffer and re-stall. */
export interface Reset {
	type: "reset";
	// The next utterance after a declared end: flush only once playback ran out at the end.
	next?: boolean;
	// Echoed in every later state message, so the main thread can tell state from before the reset.
	epoch: number;
	playing?: boolean;
}

/** Drop buffered samples at or after `timestamp`, keeping what is already due. */
export interface Truncate {
	type: "truncate";
	timestamp: Micro;
}

/** The publisher declared its audio ends here, less any codec delay, so running dry there is silence. */
export interface End {
	type: "end";
	timestamp: Micro;
}

/** Audio samples, with their buffers transferred. */
export interface Data {
	type: "data";
	data: Float32Array[];
	timestamp: Micro;
}

/**
 * Where the shared clock stands, for a buffered ring to time-stretch back onto. The clock runs at real
 * time, so one reading maps it onto the worklet's own time until the next.
 */
export interface Clock {
	type: "clock";
	// The clock's media time at context time `time`, unset while the clock has none.
	timestamp?: Micro;
	// The AudioContext's `currentTime` when the clock was read, in seconds.
	time: number;
}

/** Latency update: the floor held as decoded PCM. */
export interface Latency {
	type: "latency";
	latency: Milli;
	// When set, also hold playback, keeping what is buffered, until the next latency message without
	// it. Echoed like a reset's epoch, so state from before the park cannot report the ring playing.
	park?: number;
}

/** State update from the worklet back to the main thread. */
export interface State {
	type: "state";
	timestamp: Micro;
	stalled: boolean;
	// Whether `timestamp` is on the current timeline: it reads 0 until a frame anchors the ring.
	anchored: boolean;
	// How many times the ring has run dry mid-playback, cumulative.
	underruns: number;
	// How many times the ring skipped ahead, dropping queued audio, cumulative.
	skips: number;
	// Media those skips dropped unplayed, less declared silence, cumulative.
	trimmed: Milli;
	// Media the stretch removed by playing faster, cumulative.
	accelerated: Milli;
	// Whether playback is at a declared end, reached or announced before any audio, and nothing has played since.
	silent: boolean;
	// The epoch of the last reset or park the worklet applied.
	epoch: number;
}

/** Release or hold consumption without discarding the queued opening. */
export interface Playing {
	type: "playing";
	playing: boolean;
	epoch: number;
}
