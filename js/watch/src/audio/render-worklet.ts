import type { Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";
import { SharedRingBuffer } from "./shared-ring-buffer";

class Render extends AudioWorkletProcessor {
	// Exactly one of these is set after init, depending on which path the main thread chose.
	#shared?: SharedRingBuffer;
	#post?: AudioRingBuffer;
	#underflow = 0;
	#stateCounter = 0;

	constructor() {
		super();

		this.port.onmessage = (event: MessageEvent<Message>) => {
			const msg = event.data;
			if (msg.type === "init-shared") {
				console.log("[audio-worklet] init-shared: using SharedArrayBuffer path");
				this.#shared = new SharedRingBuffer(msg);
				this.#post = undefined;
				this.#underflow = 0;
			} else if (msg.type === "init-post") {
				console.log("[audio-worklet] init-post: using postMessage path");
				this.#post = new AudioRingBuffer(msg);
				this.#shared = undefined;
				this.#underflow = 0;
			} else if (msg.type === "data") {
				// Only meaningful in post mode.
				this.#post?.write(msg.timestamp, msg.data);
			} else if (msg.type === "latency") {
				// Only meaningful in post mode.
				this.#post?.resize(msg.latency);
			}
		};
	}

	process(_inputs: Float32Array[][], outputs: Float32Array[][], _parameters: Record<string, Float32Array>) {
		const output = outputs[0];
		const buffer = this.#shared ?? this.#post;
		const samplesRead = buffer?.read(output) ?? 0;

		if (samplesRead < output[0].length) {
			this.#underflow += output[0].length - samplesRead;
		} else if (this.#underflow > 0 && buffer) {
			console.debug(`audio underflow: ${Math.round((1000 * this.#underflow) / buffer.rate)}ms`);
			this.#underflow = 0;
		}

		// In post mode the main thread can't read worklet state directly, so we
		// periodically ship it across via postMessage. In shared mode the main
		// thread polls the shared control array directly.
		if (this.#post) {
			this.#stateCounter++;
			if (this.#stateCounter >= 5) {
				this.#stateCounter = 0;
				const state: State = {
					type: "state",
					timestamp: this.#post.timestamp,
					stalled: this.#post.stalled,
				};
				this.port.postMessage(state);
			}
		}

		return true;
	}
}

registerProcessor("render", Render);
