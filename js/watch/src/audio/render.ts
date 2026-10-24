import type { Micro, Milli } from "@moq/clock";

/** Everything the main thread sends the render worklet over its port. */
export type Message = Init | Data | Latency | Park | Reset | Truncate;

/** Create the worklet's ring. */
export interface Init {
	type: "init";
	channels: number;
	rate: number;
	latency: Milli;
	// Buffered mode: anchor to the first frame and play through. The ring holds the floor; the
	// lookahead above it is held back upstream (the main thread applies the backpressure).
	buffered: boolean;
}

/** Flush the buffer and re-stall. */
export interface Reset {
	type: "reset";
	// Echoed in every later state message, so the main thread can tell state from before the reset.
	epoch: number;
}

/** Hold playback until the next data message, keeping what is buffered. */
export interface Park {
	type: "park";
	// Echoed like a reset's, so state from before the park cannot report the ring playing.
	epoch: number;
}

/** Drop buffered samples at or after `timestamp`, keeping what is already due. */
export interface Truncate {
	type: "truncate";
	timestamp: Micro;
}

/** Audio samples, with their buffers transferred. */
export interface Data {
	type: "data";
	data: Float32Array[];
	timestamp: Micro;
}

/** Latency update: the floor held as decoded PCM. */
export interface Latency {
	type: "latency";
	latency: Milli;
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
	// The epoch of the last reset or park the worklet applied.
	epoch: number;
}
