// Sweep armed timers x timers made ready, with independent and shared abort scopes.
import { Manual, Nano, sleep } from "../src/index.ts";

console.log("mode,scope,armed,ready,arm_us,advance_us,cancel_us");
for (const mode of ["jump", "advance"]) {
	for (const scope of ["individual", "shared"]) {
		for (const armed of [100, 1000, 10000]) {
			for (const ready of [1, 10, 100, 1000].filter((value) => value <= armed)) {
				const samples: number[][] = [[], [], []];
				for (let round = 0; round < 6; round++) {
					const clock = new Manual();
					const restore = clock.install();
					const aborts = Array.from({ length: scope === "shared" ? 1 : armed }, () => new AbortController());
					try {
						const start = performance.now();
						const timers = Array.from({ length: armed }, (_, index) =>
							sleep(Nano(index < ready ? index + 1 : armed + 1), {
								signal: aborts[index % aborts.length].signal,
							}).catch(() => {}),
						);
						const armedAt = performance.now();
						if (mode === "jump") clock.jump(Nano(ready));
						else await clock.advance(Nano(ready));
						const advancedAt = performance.now();
						for (const abort of aborts) abort.abort();
						const cancelledAt = performance.now();
						await Promise.all(timers);
						if (round > 0) {
							samples[0].push((armedAt - start) * 1000);
							samples[1].push((advancedAt - armedAt) * 1000);
							samples[2].push((cancelledAt - advancedAt) * 1000);
						}
					} finally {
						restore();
					}
				}
				console.log(
					[mode, scope, armed, ready, ...samples.map((values) => values.sort((a, b) => a - b)[2])].join(","),
				);
			}
		}
	}
}
