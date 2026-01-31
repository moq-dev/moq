import type { Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";

class Render extends AudioWorkletProcessor {
	#buffer?: AudioRingBuffer;
	#underflow = 0;
	#stateCounter = 0;

	constructor() {
		super();

		// Listen for audio data from main thread
		this.port.onmessage = (event: MessageEvent<Message>) => {
			const { type } = event.data;
			if (type === "init") {
				this.#buffer = new AudioRingBuffer(event.data);
				this.#underflow = 0;
			} else if (type === "data") {
				if (!this.#buffer) throw new Error("buffer not initialized");
				this.#buffer.write(event.data.timestamp, event.data.data);
			} else if (type === "latency") {
				if (!this.#buffer) throw new Error("buffer not initialized");
				this.#buffer.resize(event.data.latency);
			} else {
				const exhaustive: never = type;
				throw new Error(`unknown message type: ${exhaustive}`);
			}
		};
	}

	process(_inputs: Float32Array[][], outputs: Float32Array[][], _parameters: Record<string, Float32Array>) {
		const output = outputs[0];
		const samplesRead = this.#buffer?.read(output) ?? 0;

		if (samplesRead < output[0].length) {
			this.#underflow += output[0].length - samplesRead;
		} else if (this.#underflow > 0 && this.#buffer) {
			console.warn(`audio underflow: ${Math.round((1000 * this.#underflow) / this.#buffer.rate)}ms`);
			this.#underflow = 0;
		}

		// Send state update every ~5 frames (~60/sec) to avoid excessive DOM updates
		this.#stateCounter++;
		if (this.#buffer && this.#stateCounter >= 5) {
			this.#stateCounter = 0;
			const state: State = {
				type: "state",
				timestamp: this.#buffer.timestamp,
				stalled: this.#buffer.stalled,
			};
			this.port.postMessage(state);
		}

		return true;
	}
}

registerProcessor("render", Render);
