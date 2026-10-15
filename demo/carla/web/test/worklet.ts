import { mock } from "bun:test";

// Bun cannot load the blob-URL worklet import used by the audio decoder, and these tests never create an AudioContext.
mock.module("../../../../js/watch/src/audio/render-worklet.ts?worklet", () => ({
	default: async () => "blob:unused-worklet",
}));
