import type { Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";

// Samples to fade over when playback stops or resumes short of a full quantum. A shortfall
// otherwise steps straight to silence and back, which is an audible click on every underrun.
const RAMP = 64;

class Render extends AudioWorkletProcessor {
	// Created by the main thread's init message.
	#ring?: AudioRingBuffer;
	#underflow = 0;
	#stateCounter = 0;
	#epoch = 0;
	// Whether the previous quantum ended short, so the next one fades back in.
	#short = false;

	constructor() {
		super();

		this.port.onmessage = (event: MessageEvent<Message>) => {
			const msg = event.data;
			if (msg.type === "init") {
				this.#ring = new AudioRingBuffer(msg);
				this.#underflow = 0;
				this.#short = false;
			} else if (msg.type === "data") {
				this.#ring?.write(msg.timestamp, msg.data);
			} else if (msg.type === "latency") {
				this.#ring?.resize(msg.latency);
			} else if (msg.type === "truncate") {
				this.#ring?.truncate(msg.timestamp);
			} else if (msg.type === "reset") {
				this.#ring?.reset();
				this.#epoch = msg.epoch;
			}
		};
	}

	process(_inputs: Float32Array[][], outputs: Float32Array[][], _parameters: Record<string, Float32Array>) {
		const output = outputs[0];
		const ring = this.#ring;
		const samplesRead = ring?.read(output) ?? 0;

		if (samplesRead < output[0].length) {
			// Fade the tail of what we did read down to the silence that follows it.
			for (const channel of output) {
				const ramp = Math.min(RAMP, samplesRead);
				for (let i = 0; i < ramp; i++) {
					channel[samplesRead - ramp + i] *= 1 - (i + 1) / ramp;
				}
			}
			this.#underflow += output[0].length - samplesRead;
			this.#short = true;
		} else {
			if (this.#short) {
				// Fade back in from the silence the shortfall left behind.
				const ramp = Math.min(RAMP, samplesRead);
				for (const channel of output) {
					for (let i = 0; i < ramp; i++) {
						channel[i] *= (i + 1) / ramp;
					}
				}
				this.#short = false;
			}
			if (this.#underflow > 0 && ring) {
				console.debug(`audio underflow: ${Math.round((1000 * this.#underflow) / ring.rate)}ms`);
				this.#underflow = 0;
			}
		}

		// The main thread can't read worklet state directly, so ship it across periodically.
		if (ring) {
			this.#stateCounter++;
			if (this.#stateCounter >= 5) {
				this.#stateCounter = 0;
				const state: State = {
					type: "state",
					timestamp: ring.timestamp,
					stalled: ring.stalled,
					underruns: ring.underruns,
					epoch: this.#epoch,
				};
				this.port.postMessage(state);
			}
		}

		return true;
	}
}

registerProcessor("render", Render);
