import type { WorkletFrame } from "./capture";

// Posts each render quantum to the main thread with the wall time it was rendered at: the audio
// thread runs on time while the page may not, and `Date.now()` is the one clock a worklet has.
class Capture extends AudioWorkletProcessor {
	process(input: Float32Array[][]) {
		if (input.length > 1) throw new Error("only one input is supported.");

		const channels = input[0];
		if (channels.length === 0) return true; // TODO: No input hooked up?

		// biome-ignore lint/style/noRestrictedGlobals: a worklet has no `performance`, so no `@moq/clock`.
		const msg: WorkletFrame = { channels, rendered: Date.now() };
		this.port.postMessage(msg);
		return true;
	}
}

registerProcessor("capture", Capture);
