import type { Time } from "@moq/net";
import type { SharedRingBufferInit } from "./shared-ring-buffer";

/** Everything the main thread sends the render worklet over its port. */
export type Message = InitShared | InitPost | Data | Latency | Reset | Truncate;
export type ToMain = State;

/** Init message when SharedArrayBuffer is available. */
export interface InitShared extends SharedRingBufferInit {
	type: "init-shared";
}

/** Init message for the postMessage fallback path. */
export interface InitPost {
	type: "init-post";
	channels: number;
	rate: number;
	latency: Time.Milli;
	// Buffered mode: anchor to the first frame and play through; the ring is sized to the floor and
	// the lookahead above it is held back upstream (the main thread applies the backpressure).
	buffered: boolean;
}

/** Flush the buffer and re-stall (fallback path only; shared path resets via Atomics). */
export interface Reset {
	type: "reset";
}

/**
 * Drop buffered samples at or after `timestamp`, keeping what is already due (fallback path only;
 * the shared path truncates via Atomics).
 */
export interface Truncate {
	type: "truncate";
	timestamp: Time.Micro;
}

/** Audio samples sent via postMessage (fallback path only). */
export interface Data {
	type: "data";
	data: Float32Array[];
	timestamp: Time.Micro;
}

/** Latency update sent via postMessage (fallback path only). */
export interface Latency {
	type: "latency";
	latency: Time.Milli;
}

/** State update from the worklet back to main thread (fallback path only). */
export interface State {
	type: "state";
	timestamp: Time.Micro;
	stalled: boolean;
}
