import { ComfortNoise } from "./comfort";
import type { Clock, Message, State } from "./render";
import { AudioRingBuffer } from "./ring-buffer";

// Samples to fade over when playback stops or resumes short of a full quantum. A shortfall
// otherwise steps straight to silence and back, which is an audible click on every underrun.
const RAMP = 64;

class Render extends AudioWorkletProcessor {
	// Created by the main thread's init message.
	#ring?: AudioRingBuffer;
	#comfort?: ComfortNoise;
	#underflow = 0;
	#stateCounter = 0;
	#epoch = 0;
	#playing = true;
	// Whether the previous quantum ended short, so the next one fades back in.
	#short = false;
	// The shared clock's latest reading, which the ring plays onto.
	#clock?: Clock;

	constructor() {
		super();

		this.port.onmessage = (event: MessageEvent<Message>) => {
			const msg = event.data;
			if (msg.type === "init") {
				this.#ring = new AudioRingBuffer(msg);
				this.#comfort = new ComfortNoise(msg.rate);
				this.#playing = msg.playing ?? true;
				this.#underflow = 0;
				this.#short = false;
				this.#clock = undefined;
			} else if (msg.type === "data") {
				this.#ring?.write(msg.timestamp, msg.data);
			} else if (msg.type === "received") {
				this.#ring?.received(msg.end);
			} else if (msg.type === "clock") {
				this.#clock = msg;
				if (msg.jumped) this.#ring?.jumped(msg.jumped);
			} else if (msg.type === "latency") {
				this.#ring?.setLatency(msg.latency, msg);
				// The clock moves with the delay, so a reading from before it would misplace audio.
				this.#clock = undefined;
			} else if (msg.type === "end") {
				this.#ring?.end(msg.timestamp);
			} else if (msg.type === "truncate") {
				this.#ring?.truncate(msg.timestamp);
			} else if (msg.type === "reset") {
				if (msg.next) this.#ring?.next();
				else {
					this.#ring?.reset();
					this.#comfort = this.#ring ? new ComfortNoise(this.#ring.rate) : undefined;
				}
				this.#epoch = msg.epoch;
				this.#playing = msg.playing ?? true;
				// A reset ends the timeline the clock was read on.
				this.#clock = undefined;
			} else if (msg.type === "playing" && msg.epoch === this.#epoch) {
				this.#playing = msg.playing;
			}
		};
	}

	process(_inputs: Float32Array[][], outputs: Float32Array[][], _parameters: Record<string, Float32Array>) {
		const output = outputs[0];
		const ring = this.#ring;
		const clock = this.#clock;
		ring?.clock(
			clock?.timestamp !== undefined ? clock.timestamp + (currentTime - clock.time) * 1_000_000 : undefined,
		);
		const samplesRead = this.#playing ? (ring?.read(output) ?? 0) : 0;
		// A declared silence between two buffered utterances plays as comfort noise, faded like any other.
		const quiet = this.#playing ? ring?.quiet : undefined;
		if (quiet) {
			const [from, to] = quiet;
			this.#comfort?.played(output, 0, from);
			fadeOut(output, from);
			fadeIn(output, to, samplesRead);
			this.#comfort?.fill(output, from, to);
			this.#comfort?.played(output, to, samplesRead);
		} else {
			this.#comfort?.played(output, 0, samplesRead);
		}

		if (samplesRead < output[0].length) {
			// Fade the tail of what we did read down to the silence that follows it.
			fadeOut(output, samplesRead);
			// The publisher declared this silence, including the refill before its next utterance.
			if (ring?.silent) this.#comfort?.fill(output, samplesRead);
			else if (this.#playing) this.#underflow += output[0].length - samplesRead;
			this.#short = true;
		} else {
			// Fade back in from the silence the shortfall left behind.
			if (this.#short) fadeIn(output, 0, samplesRead);
			// A declared silence running to the end of the quantum fades the next one in.
			this.#short = quiet !== undefined && quiet[1] === samplesRead;
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
					time: currentTime + output[0].length / ring.rate,
					stalled: ring.stalled,
					anchored: ring.anchored,
					underruns: ring.underruns,
					skips: ring.skips,
					trimmed: ring.trimmed,
					accelerated: ring.accelerated,
					silent: ring.silent,
					epoch: this.#epoch,
				};
				this.port.postMessage(state);
			}
		}

		return true;
	}
}

// Fade `output` down over the samples just before `at`.
function fadeOut(output: Float32Array[], at: number): void {
	const ramp = Math.min(RAMP, at);
	for (const channel of output) {
		for (let i = 0; i < ramp; i++) channel[at - ramp + i] *= 1 - (i + 1) / ramp;
	}
}

// Fade `output` up over the samples from `at`, short of `end`.
function fadeIn(output: Float32Array[], at: number, end: number): void {
	const ramp = Math.min(RAMP, end - at);
	for (const channel of output) {
		for (let i = 0; i < ramp; i++) channel[at + i] *= (i + 1) / ramp;
	}
}

registerProcessor("render", Render);
