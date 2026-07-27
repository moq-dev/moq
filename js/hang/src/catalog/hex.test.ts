import { describe, expect, it } from "bun:test";
import { AudioConfigSchema } from "./audio";
import { VideoConfigSchema } from "./video";

describe("description encoding", () => {
	const video = { codec: "avc1.42e02a", container: { kind: "legacy" as const } };
	const audio = { codec: "opus", container: { kind: "legacy" as const }, sampleRate: 48000, numberOfChannels: 2 };

	it("accepts hex", () => {
		expect(VideoConfigSchema.safeParse({ ...video, description: "0142002a" }).success).toBe(true);
		expect(AudioConfigSchema.safeParse({ ...audio, description: "4f707573486561640102" }).success).toBe(true);
	});

	// A base64 avcC ("AU...") is valid JSON and used to sail through, only to fail against a
	// remote hex decoder that has no idea which field it is talking about.
	it("rejects base64", () => {
		expect(VideoConfigSchema.safeParse({ ...video, description: "AUIAKv/hABtnQgAq" }).success).toBe(false);
		expect(AudioConfigSchema.safeParse({ ...audio, description: "T3B1c0hlYWQBAg==" }).success).toBe(false);
	});

	it("rejects an odd number of digits", () => {
		expect(VideoConfigSchema.safeParse({ ...video, description: "014" }).success).toBe(false);
	});
});
