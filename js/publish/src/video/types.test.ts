import { expect, test } from "bun:test";
import { normalizeSource, type StreamTrack } from "./types";

// A MediaStreamTrack stand-in: the check is structural, never a prototype check, so it stays
// correct for a track that came from another realm (an iframe, a worker).
const track = { kind: "video", getSettings: () => ({}) } as unknown as StreamTrack;

test("capture options preserve the track and scale", () => {
	const source = { track, scale: 2 };
	expect(normalizeSource(source)).toBe(source);
	expect(normalizeSource(track).track).toBe(track);
});
