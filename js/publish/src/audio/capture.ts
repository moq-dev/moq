import type { Time } from "@moq/net";

export interface AudioFrame {
	timestamp: Time.Micro;
	channels: Float32Array[];
}
