import type { Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";
import { SharedRingBuffer } from "./shared-ring-buffer";

class Render extends AudioWorkletProcessor {
	// Set after init, depending on which path the main thread chose.
	#backend?: SharedRingBuffer | AudioRingBuffer;
	#underflow = 0;
	#stateCounter = 0;

	constructor() {
		super();

		this.port.onmessage = (event: MessageEvent<Message>) => {
			const msg = event.data;
			if (msg.type === "init-shared") {
				console.log("[audio-worklet] init-shared: using SharedArrayBuffer path");
				this.#backend = new SharedRingBuffer(msg);
				this.#underflow = 0;
			} else if (msg.type === "init-post") {
				console.log("[audio-worklet] init-post: using postMessage path");
				this.#backend = new AudioRingBuffer(msg);
				this.#underflow = 0;
			} else if (msg.type === "data") {
				// Only meaningful in post mode.
				if (this.#backend instanceof AudioRingBuffer) this.#backend.write(msg.timestamp, msg.data);
			} else if (msg.type === "latency") {
				// Only meaningful in post mode.
				if (this.#backend instanceof AudioRingBuffer) this.#backend.resize(msg.latency);
			}
		};
	}

	process(_inputs: Float32Array[][], outputs: Float32Array[][], _parameters: Record<string, Float32Array>) {
		const output = outputs[0];
		const backend = this.#backend;
		const samplesRead = backend?.read(output) ?? 0;

		if (samplesRead < output[0].length) {
			this.#underflow += output[0].length - samplesRead;
		} else if (this.#underflow > 0 && backend) {
			console.debug(`audio underflow: ${Math.round((1000 * this.#underflow) / backend.rate)}ms`);
			this.#underflow = 0;
		}

		// In post mode the main thread can't read worklet state directly, so we
		// periodically ship it across via postMessage. In shared mode the main
		// thread reads the shared control array directly.
		if (backend instanceof AudioRingBuffer) {
			this.#stateCounter++;
			if (this.#stateCounter >= 5) {
				this.#stateCounter = 0;
				const state: State = {
					type: "state",
					timestamp: backend.timestamp,
					stalled: backend.stalled,
				};
				this.port.postMessage(state);
			}
		}

		return true;
	}
}

registerProcessor("render", Render);
