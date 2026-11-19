import type { WorkletFrame } from "./capture";

// Posts each render quantum to the main thread, which stamps it: see `Stamper`.
class Capture extends AudioWorkletProcessor {
	process(input: Float32Array[][]) {
		if (input.length > 1) throw new Error("only one input is supported.");

		const channels = input[0];
		if (channels.length === 0) return true; // TODO: No input hooked up?

		const msg: WorkletFrame = { channels };
		this.port.postMessage(msg);
		return true;
	}
}

registerProcessor("capture", Capture);
