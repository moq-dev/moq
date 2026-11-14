/**
 * Everything the main thread sends the render worklet over its port. Structured clone carries no
 * clock classes, so latencies are plain milliseconds and timestamps plain microseconds.
 */
export type Message = Init | Data | Latency | Reset | Truncate | Playing | End | Clock | Received;

/** Create the worklet's ring. */
export interface Init extends Terms {
	type: "init";
	channels: number;
	rate: number;
	playing?: boolean;
}

/**
 * What the ring holds and how far it lets audio trail the clock. Only the floor is decoded PCM: the
 * rest of the target, and in buffered mode the lookahead, stays encoded upstream (the main thread
 * applies the backpressure).
 */
export interface Terms {
	// The floor held as decoded PCM, which playback starts on.
	latency: number;
	// How far audio may trail the shared clock before the ring snaps onto it: the `stretch` threshold,
	// or for a buffered player the larger of it and the lookahead. Zero turns the time stretch off.
	bound: number;
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
	timestamp: number;
}

/** The publisher declared its audio ends here, less any codec delay, so running dry there is silence. */
export interface End {
	type: "end";
	timestamp: number;
}

/** Audio samples, with their buffers transferred. */
export interface Data {
	type: "data";
	data: Float32Array[];
	timestamp: number;
}

/** Latency update: new terms, and where the shared clock they move now stands, if there is one. */
export interface Latency extends Terms {
	type: "latency";
	clock?: Reading;
}

/**
 * Where the shared clock stands, for the ring to time-stretch onto. The clock runs at real time, so one
 * reading maps it onto the worklet's own time until the next.
 */
export interface Reading {
	// The clock's media time at context time `time`, in the ring's coordinates, unset while it has none.
	timestamp?: number;
	// The AudioContext's `currentTime` when the clock was read, in seconds.
	time: number;
}

/** A new reading of the shared clock. */
export interface Clock extends Reading {
	type: "clock";
}

/** Media arrived ending at `end`, in the ring's coordinates, decoded or not. */
export interface Received {
	type: "received";
	end: number;
}

/** State update from the worklet back to the main thread. */
export interface State {
	type: "state";
	timestamp: number;
	stalled: boolean;
	// Whether `timestamp` is on the current timeline: it reads 0 until a frame anchors the ring.
	anchored: boolean;
	// How many times the ring has run dry mid-playback, cumulative.
	underruns: number;
	// How many times the ring skipped ahead, dropping queued audio, cumulative.
	skips: number;
	// Media those skips dropped unplayed, less declared silence, cumulative.
	trimmed: number;
	// Media the stretch removed by playing faster, cumulative.
	accelerated: number;
	// Whether playback is at a declared end, reached or announced before any audio, and nothing has played since.
	silent: boolean;
	// The epoch of the last reset the worklet applied.
	epoch: number;
}

/** Release or hold consumption without discarding the queued opening. */
export interface Playing {
	type: "playing";
	playing: boolean;
	epoch: number;
}
