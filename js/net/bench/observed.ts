/** Compare eager and on-demand forwarding over connections and readers per connection. */
import { Computed, Effect, type Getter, Signal } from "@moq/signals";
import { Follow, Observed } from "../src/util/observed";

const settle = () => new Promise<void>((resolve) => setImmediate(resolve));
console.log("forwarding,connections,readers,held_estimates,update_us");
for (const mode of ["eager", "observed"] as const)
	for (const connections of [1, 32, 256])
		for (const readers of [0, 1, 8, 64]) {
			const values = Array.from({ length: connections }, () => new Observed(0));
			const eager: Computed<number>[] = [];
			let updates = 0;
			const effects = values.flatMap((value) => {
				let forwarded: Getter<number | undefined>;
				if (mode === "eager") {
					const computed = new Computed((effect) => effect.get(value));
					eager.push(computed);
					forwarded = computed;
				} else {
					forwarded = new Follow(new Signal<Getter<number> | undefined>(value));
				}
				return Array.from(
					{ length: readers },
					() =>
						new Effect((effect) => {
							effect.get(forwarded);
							updates++;
						}),
				);
			});
			await settle();
			const held = values.filter((value) => value.wanted.peek()).length;
			const samples: number[] = [];
			for (let round = 0; round < 6; round++) {
				const before = updates;
				const start = performance.now();
				for (let i = 1; i <= 128; i++) {
					values[0].set(round * 128 + i);
					await settle();
				}
				if (updates - before !== 128 * readers)
					throw new Error("updated an unrelated connection or lost a reader");
				if (round > 0) samples.push(((performance.now() - start) * 1000) / 128);
			}
			samples.sort((a, b) => a - b);
			console.log(`${mode},${connections},${readers},${held},${samples[2].toFixed(2)}`);
			for (const effect of effects) effect.close();
			for (const computed of eager) computed.close();
			await settle();
		}
