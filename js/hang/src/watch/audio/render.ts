import type { Time } from "@moq/lite";

export type Message = Init | Data;

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
