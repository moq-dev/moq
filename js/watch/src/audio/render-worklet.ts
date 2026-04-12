import type { Message } from "./render";
import { SharedRingBuffer } from "./shared-ring-buffer";

class Render extends AudioWorkletProcessor {
	#buffer?: SharedRingBuffer;

	constructor() {
		super();

		this.port.onmessage = (event: MessageEvent<Message>) => {
			if (event.data.type === "init") {
				this.#buffer = new SharedRingBuffer(event.data);
			}
		};
	}

	process(_inputs: Float32Array[][], outputs: Float32Array[][], _parameters: Record<string, Float32Array>) {
		this.#buffer?.read(outputs[0]);
		return true;
	}
}

registerProcessor("render", Render);
