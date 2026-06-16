import type { Time } from "@moq/wasm";

export interface AudioFrame {
	timestamp: Time.Micro;
	channels: Float32Array[];
}
