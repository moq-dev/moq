/** Sweep sparse arrival order and rolling expiration against the retained gap count. */
import { Instant, Milli, Nano } from "@moq/clock";
import { Ledger } from "../src/ledger.ts";

/** Measure each arrival with the expiration and coverage checks its consumers perform. */
export function run(): void {
	console.log("arrivals,order,ns_per_arrival");
	for (const count of [2048, 8192, 32768]) {
		for (const order of ["ascending", "descending", "shuffled", "rolling"]) {
			const sequences = Array.from({ length: count }, (_, i) => i * 2);
			if (order === "descending") sequences.reverse();
			if (order === "shuffled") {
				let seed = 42;
				for (let i = count - 1; i > 0; i--) {
					seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
					const j = seed % (i + 1);
					[sequences[i], sequences[j]] = [sequences[j], sequences[i]];
				}
			}
			const ledger = new Ledger();
			const grace = Milli(count);
			const start = performance.now();
			for (let i = 0; i < count * (order === "rolling" ? 2 : 1); i++) {
				const sequence = order === "rolling" ? i * 2 : sequences[i];
				const at = order === "rolling" ? Instant(Nano.fromMilli(Milli(i))) : Instant.zero;
				ledger.account(sequence, sequence + 1, at);
				ledger.expire(at, grace);
				if (!ledger.covers(sequence, sequence + 1)) throw new Error("arrival missing");
			}
			const arrivals = count * (order === "rolling" ? 2 : 1);
			console.log(`${count},${order},${(((performance.now() - start) * 1e6) / arrivals).toFixed(0)}`);
			ledger.expire(Instant(Nano.fromMilli(Milli(count * 3))), grace);
			if (!ledger.covers(0, arrivals * 2 - 1) || ledger.hasGaps()) throw new Error("expiration incomplete");
		}
	}
}

if (import.meta.main) run();
