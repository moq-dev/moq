import type { WorkletFrame } from "./capture";

class Capture extends AudioWorkletProcessor {
	#sampleCount = 0;
	// Microseconds on the shared clock when the worklet started.
	#zero: number;

	constructor(options?: AudioWorkletNodeOptions) {
		super();
		this.#zero = options?.processorOptions?.zero ?? 0;
	}

	process(input: Float32Array[][]) {
		if (input.length > 1) throw new Error("only one input is supported.");

		const channels = input[0];
		if (channels.length === 0) return true; // TODO: No input hooked up?

		// Convert sample count to microseconds, offset onto the shared wall clock.
		const timestamp = (this.#sampleCount / sampleRate) * 1_000_000 + this.#zero;

		const msg: WorkletFrame = {
			timestamp,
			channels,
		};

		this.port.postMessage(msg);

		this.#sampleCount += channels[0].length;
		return true;
	}
}

registerProcessor("capture", Capture);
