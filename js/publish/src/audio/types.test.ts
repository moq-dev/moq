import { describe, expect, test } from "bun:test";
import {
	isSampleSource,
	normalizeSource,
	type SampleSource,
	type SourceConfig,
	type StreamTrack,
	sourceKind,
} from "./types";

// A MediaStreamTrack stand-in: the helpers only ever check the shape, never the prototype, so they
// stay correct for a track that came from another realm (an iframe, a worker).
const track = { kind: "audio" } as unknown as StreamTrack;

const samples: SampleSource = {
	samples: new ReadableStream<AudioData>(),
	sampleRate: 44_100,
	channelCount: 2,
};

describe("isSampleSource", () => {
	test("separates decoded samples from a capture track", () => {
		expect(isSampleSource(samples)).toBe(true);
		expect(isSampleSource(track)).toBe(false);
		expect(isSampleSource({ track, kind: "voice" })).toBe(false);
	});
});

describe("sourceKind", () => {
	test("reads the kind off either source shape", () => {
		expect(sourceKind(samples)).toBeUndefined();
		expect(sourceKind({ track, kind: "voice" })).toBe("voice");
		expect(sourceKind({ ...samples, kind: "music" })).toBe("music");
	});

	// A bare track says nothing about its content, so the encoder leaves the Opus knobs alone.
	test("a bare track has no kind", () => {
		expect(sourceKind(track)).toBeUndefined();
	});
});

describe("normalizeSource", () => {
	test("gives a bare track no kind", () => {
		expect(normalizeSource(track)).toEqual({ track });
	});

	test("passes a config through untouched", () => {
		const config: SourceConfig = { track, kind: "music" };
		expect(normalizeSource(config)).toBe(config);
	});
});
