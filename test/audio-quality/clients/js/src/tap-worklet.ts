/**
 * The harness's own AudioWorklet, fanned out from the player's output: it listens and never plays.
 *
 * It sees every render quantum rather than a 250 ms sample of them, which is what lets a gap of a few
 * milliseconds between two probe samples be counted at all. See `quantum.ts` for how a quantum is
 * read.
 *
 * @module
 */
import { classify, Ledger } from "./quantum.ts";
import type { TapInit, TapReport } from "./tap.ts";

/** Quanta between reports: about 170 ms at 48 kHz, inside the probe's 250 ms. */
const REPORT_EVERY = 64;

class Tap extends AudioWorkletProcessor {
	#ledger: Ledger;
	#since = 0;

	constructor(options: AudioWorkletNodeOptions) {
		super();
		const init = options.processorOptions as TapInit;
		this.#ledger = new Ledger(sampleRate, init.floor);
		// The run is over: close a gap still open and report at once, rather than lose it.
		this.port.onmessage = () => {
			this.#ledger.finish();
			this.#report();
		};
	}

	#report(): void {
		this.#since = 0;
		const report: TapReport = { counts: { ...this.#ledger.counts }, gaps: this.#ledger.take() };
		this.port.postMessage(report);
	}

	process(inputs: Float32Array[][]): boolean {
		const input = inputs[0] ?? [];
		this.#ledger.add(currentFrame, input[0]?.length ?? 128, classify(input));

		if (++this.#since >= REPORT_EVERY) this.#report();
		return true;
	}
}

registerProcessor("audio-quality-tap", Tap);
