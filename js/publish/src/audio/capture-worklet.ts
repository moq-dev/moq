import { Time } from "@moq/net";
import type { AudioFrame } from "./capture";

class Capture extends AudioWorkletProcessor {
	#sampleCount = 0;
	#zero: Time.Micro;

	constructor(options?: AudioWorkletNodeOptions) {
		super();
		this.#zero = (options?.processorOptions?.zero ?? 0) as Time.Micro;
	}

	process(input: Float32Array[][]) {
		if (input.length > 1) throw new Error("only one input is supported.");

		const channels = input[0];
		if (channels.length === 0) return true; // TODO: No input hooked up?

		// Convert sample count to microseconds, offset onto the shared wall clock.
		const timestamp = (Time.Micro.fromSecond((this.#sampleCount / sampleRate) as Time.Second) +
			this.#zero) as Time.Micro;

		const msg: AudioFrame = {
			timestamp,
			channels,
		};

		this.port.postMessage(msg);

		this.#sampleCount += channels[0].length;
		return true;
	}
}

registerProcessor("capture", Capture);
