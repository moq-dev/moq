// The gate route's per-frame logic as the publisher would carry it: decide, pre-roll, and stamp from a queue.
export class Gate {
	#hang = 0;
	#held: { pcm: Float32Array; ts: number } | undefined;
	#stamps: number[] = [];
	constructor(
		readonly threshold = 10 ** (-45 / 10),
		readonly hangover = 10,
	) {}
	push(pcm: Float32Array, ts: number, muted: boolean): { pcm: Float32Array; ts: number }[] {
		let sum = 0;
		for (const v of pcm) sum += v * v;
		const loud = !muted && sum / pcm.length > this.threshold;
		const out: { pcm: Float32Array; ts: number }[] = [];
		if (loud && this.#hang === 0 && this.#held) out.push(this.#held);
		if (loud) this.#hang = this.hangover + 1;
		if (this.#hang > 0) out.push({ pcm, ts });
		this.#hang = Math.max(0, this.#hang - 1);
		this.#held = this.#hang > 0 ? undefined : { pcm, ts };
		for (const f of out) this.#stamps.push(f.ts);
		return out;
	}
	stamp(): number | undefined {
		return this.#stamps.shift();
	}
}
