import { describe, expect, test } from "bun:test";
import { type FrameSource, isStreamTrack, type StreamTrack } from "./types";

// A MediaStreamTrack stand-in: the check is structural, never a prototype check, so it stays
// correct for a track that came from another realm (an iframe, a worker).
const track = { kind: "video", getSettings: () => ({}) } as unknown as StreamTrack;

const frames: FrameSource = {
	frames: new ReadableStream<VideoFrame>(),
	frameRate: 24,
};

describe("isStreamTrack", () => {
	test("separates a frame stream from a capture track", () => {
		expect(isStreamTrack(track)).toBe(true);
		expect(isStreamTrack(frames)).toBe(false);
	});

	// frameRate is optional, so a source without one must still be recognized.
	test("recognizes a frame stream with no declared rate", () => {
		expect(isStreamTrack({ frames: new ReadableStream<VideoFrame>() })).toBe(false);
	});
});
