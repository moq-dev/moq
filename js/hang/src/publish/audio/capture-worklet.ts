import { Time } from "@moq/lite";
import type { AudioFrame } from "./capture";

class Capture extends AudioWorkletProcessor {
	#sampleCount = 0;

	process(input: Float32Array[][]) {
		if (input.length > 1) throw new Error("only one input is supported.");

		const channels = input[0];
		if (channels.length === 0) return true; // TODO: No input hooked up?

		// Convert sample count to microseconds
		const timestamp = Time.Micro.fromSecond((this.#sampleCount / sampleRate) as Time.Second);

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
