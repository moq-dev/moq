/** Time creating and closing an Effect, bare and with a tracked signal, against a bare `new Error()`. */
import { Effect, Signal } from "../src/index.ts";

const runs = 20_000;
const reps = 9;
const signal = new Signal(0);
let checksum = 0;

const ops: [string, () => void][] = [
	// The creation stack each Effect captures for its diagnostics; the floor for the rows below.
	[
		"new-error",
		() => {
			checksum += new Error("created here:").message.length;
		},
	],
	[
		"create-close",
		() => {
			new Effect().close();
			checksum++;
		},
	],
	[
		"create-get-close",
		() => {
			const effect = new Effect((effect) => {
				checksum += effect.get(signal) + 1;
			});
			effect.close();
		},
	],
];

// Nanoseconds per op for the fastest of several reps, since a slower one measured the machine.
function time(body: () => void): number {
	let best = Number.POSITIVE_INFINITY;
	for (let rep = 0; rep < reps; rep++) {
		const start = performance.now();
		for (let i = 0; i < runs; i++) body();
		best = Math.min(best, ((performance.now() - start) * 1e6) / runs);
	}
	return best;
}

// Run every op once first, so the JIT has seen all of them and the first row isn't timing warmup.
for (const [, body] of ops) for (let i = 0; i < runs; i++) body();

console.log("op,ns_per_op");
for (const [op, body] of ops) console.log(`${op},${time(body).toFixed(0)}`);
if (checksum === 0) throw new Error("benchmark did no work");
