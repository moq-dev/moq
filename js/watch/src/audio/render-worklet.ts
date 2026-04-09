import type { Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";
import { SharedRingBuffer } from "./shared-ring-buffer";

class Render extends AudioWorkletProcessor {
	#buffer?: AudioRingBuffer;
	#shared?: SharedRingBuffer;
	#underflow = 0;
	#stateCounter = 0;

	constructor() {
		super();

		// Listen for messages from the main thread.
		this.port.onmessage = (event: MessageEvent<Message>) => {
			const { type } = event.data;
			if (type === "init") {
				this.#buffer = new AudioRingBuffer(event.data);
				this.#shared = undefined;
				this.#underflow = 0;
			} else if (type === "shared-init") {
				this.#shared = new SharedRingBuffer(event.data);
				this.#buffer = undefined;
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
		const active = this.#shared ?? this.#buffer;
		const samplesRead = active?.read(output) ?? 0;

		if (samplesRead < output[0].length) {
			this.#underflow += output[0].length - samplesRead;
		} else if (this.#underflow > 0) {
			console.debug(`audio underflow: ${Math.round((1000 * this.#underflow) / sampleRate)}ms`);
			this.#underflow = 0;
		}

		// Send state updates for the postMessage path.
		// The shared path doesn't need this — the main thread reads state from shared memory.
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
