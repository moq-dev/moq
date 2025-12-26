import type { Time } from "@moq/lite";

export interface AudioFrame {
	timestamp: Time.Micro;
	channels: Float32Array[];
}
