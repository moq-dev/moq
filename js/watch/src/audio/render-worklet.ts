import type { Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";
import { SharedRingBuffer } from "./shared-ring-buffer";

// Samples to fade over when playback stops or resumes short of a full quantum. A shortfall
// otherwise steps straight to silence and back, which is an audible click on every underrun.
const RAMP = 64;

class Render extends AudioWorkletProcessor {
	// Set after init, depending on which path the main thread chose.
	#backend?: SharedRingBuffer | AudioRingBuffer;
	#underflow = 0;
	#stateCounter = 0;
	// Whether the previous quantum ended short, so the next one fades back in.
	#short = false;

	constructor() {
		super();

		this.port.onmessage = (event: MessageEvent<Message>) => {
			const msg = event.data;
			if (msg.type === "init-shared") {
				console.log("[audio-worklet] init-shared: using SharedArrayBuffer path");
				const previous = this.#backend instanceof SharedRingBuffer ? this.#backend : undefined;
				this.#backend = new SharedRingBuffer(msg, previous);
				this.#underflow = 0;
				this.#short = false;
			} else if (msg.type === "init-post") {
				console.log("[audio-worklet] init-post: using postMessage path");
				this.#backend = new AudioRingBuffer(msg);
				this.#underflow = 0;
				this.#short = false;
			} else if (msg.type === "data") {
				// Only meaningful in post mode.
				if (this.#backend instanceof AudioRingBuffer) this.#backend.write(msg.timestamp, msg.data);
			} else if (msg.type === "latency") {
				// Only meaningful in post mode.
				if (this.#backend instanceof AudioRingBuffer) this.#backend.resize(msg.latency);
			} else if (msg.type === "truncate") {
				// Only meaningful in post mode; shared mode truncates via the control array.
				if (this.#backend instanceof AudioRingBuffer) this.#backend.truncate(msg.timestamp);
			} else if (msg.type === "reset") {
				// Only meaningful in post mode; shared mode resets via the control array.
				if (this.#backend instanceof AudioRingBuffer) this.#backend.reset();
			} else if (msg.type === "stall") {
				// Only meaningful in post mode; shared mode stalls via the control array.
				if (this.#backend instanceof AudioRingBuffer) this.#backend.stall();
			}
		};
	}

	process(_inputs: Float32Array[][], outputs: Float32Array[][], _parameters: Record<string, Float32Array>) {
		const output = outputs[0];
		const backend = this.#backend;
		const samplesRead = backend?.read(output) ?? 0;

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
				for (const channel of output) {
					for (let i = 0; i < Math.min(RAMP, samplesRead); i++) {
						channel[i] *= (i + 1) / RAMP;
					}
				}
				this.#short = false;
			}
			if (this.#underflow > 0 && backend) {
				console.debug(`audio underflow: ${Math.round((1000 * this.#underflow) / backend.rate)}ms`);
				this.#underflow = 0;
			}
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
					underruns: backend.underruns,
				};
				this.port.postMessage(state);
			}
		}

		return true;
	}
}

registerProcessor("render", Render);
