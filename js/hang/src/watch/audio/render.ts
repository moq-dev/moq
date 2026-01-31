import type { Time } from "@moq/lite";

export type Message = Init | Data | Latency;

export type ToMain = State;

export interface State {
	type: "state";
	timestamp: Time.Micro;
	stalled: boolean;
}

export interface Data {
	type: "data";
	data: Float32Array[];
	timestamp: Time.Micro;
}

export interface Init {
	type: "init";
	rate: number;
	channels: number;
	latency: Time.Milli;
}

export interface Latency {
	type: "latency";
	latency: Time.Milli;
}
