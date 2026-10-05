/**
 * What one render quantum at the player's output says about the ring behind it.
 *
 * The player's render worklet copies what its ring holds into the front of each output quantum and
 * leaves the rest as the zeros Web Audio hands it. So a ring that ran dry shows up at the output as
 * exact zeros at the end of a quantum across every channel, and an empty or stalled ring as a quantum
 * of nothing but zeros. The published tone never lands on exact zero in every channel at once, which
 * is what makes the output readable as the ring's fill level without a counter in the player.
 *
 * Free of worklet globals so the classification is unit tested rather than trusted.
 *
 * @module
 */
import type { Gap } from "./schema.ts";

/** How the ring filled one quantum. */
export type Fill = "full" | "short" | "silent";

/** One quantum, classified. */
export type Quantum = {
	fill: Fill;
	/** Samples per channel the ring did not supply: the trailing zeros, or the whole quantum. */
	missing: number;
	/** Mean square over every sample of every channel. */
	power: number;
};

/** Classify one quantum of planar audio. No channels at all is a quantum that played nothing. */
export function classify(channels: Float32Array[]): Quantum {
	const length = channels[0]?.length ?? 0;
	if (length === 0) return { fill: "silent", missing: 0, power: 0 };

	let last = -1;
	let sum = 0;
	for (const channel of channels) {
		for (let i = 0; i < channel.length; i++) {
			const value = channel[i] ?? 0;
			if (value !== 0 && i > last) last = i;
			sum += value * value;
		}
	}
	const power = sum / (length * channels.length);
	if (last < 0) return { fill: "silent", missing: length, power };

	const missing = length - 1 - last;
	return { fill: missing > 0 ? "short" : "full", missing, power };
}

/** Cumulative counts since the first audible quantum. */
export type Counts = {
	/** Quanta rendered. */
	quanta: number;
	/** Quanta under the silence floor, whatever the reason. */
	quiet: number;
};

/**
 * Folds quanta into {@link Counts}, and closes a {@link Gap} each time the ring fills a quantum again.
 *
 * Nothing before the first non-silent quantum counts: the ring is filling for the first time, which
 * is the tune-in, not a gap.
 */
export class Ledger {
	readonly counts: Counts = { quanta: 0, quiet: 0 };
	readonly #rate: number;
	readonly #floor: number;
	#started = false;
	#open: { frame: number; missing: number; quanta: number; short: number } | undefined;
	#closed: Gap[] = [];

	/** `rate` is the context's sample rate; `floor` is the silence floor as an RMS level. */
	constructor(rate: number, floor: number) {
		this.#rate = rate;
		this.#floor = floor;
	}

	/** Account for one quantum of `length` samples that started at `frame` on the render clock. */
	add(frame: number, length: number, quantum: Quantum): void {
		if (!this.#started) {
			if (quantum.fill === "silent") return;
			this.#started = true;
		}

		this.counts.quanta++;
		if (Math.sqrt(quantum.power) < this.#floor) this.counts.quiet++;
		if (quantum.fill === "full") {
			this.#close();
			return;
		}

		// A short quantum inside an open gap is a ring that got a little and ran out again: still one
		// gap, since only a full quantum ends it.
		this.#open ??= { frame: frame + length - quantum.missing, missing: 0, quanta: 0, short: 0 };
		const open = this.#open;
		open.missing += quantum.missing;
		open.quanta++;
		if (quantum.fill === "short") open.short++;
	}

	/** Gaps closed since the last call. A gap still open is reported once a full quantum ends it. */
	take(): Gap[] {
		return this.#closed.splice(0, this.#closed.length);
	}

	/** Close a gap still open, for the next {@link take}: the measurement ended inside it. */
	finish(): void {
		this.#close();
	}

	#close(): void {
		if (!this.#open) return;
		const ms = (samples: number) => (samples / this.#rate) * 1000;
		const { frame, missing, quanta, short } = this.#open;
		this.#closed.push({ at: ms(frame), ms: ms(missing), quanta, short });
		this.#open = undefined;
	}
}
