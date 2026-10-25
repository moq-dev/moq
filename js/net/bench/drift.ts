/** Sweep publishers, readers, and retained history for a parked frame read's drift check. */
import { Manual, Milli } from "@moq/clock";
import { type Group, Track } from "@moq/net";

/** Read live frames after a retained window, checking every delivered payload. */
export async function run(): Promise<void> {
	const clock = new Manual();
	const installed = clock.install();
	const payload = new Uint8Array([7]);
	try {
		console.log("retained,publishers,readers,ns_per_frame");
		for (const retained of [1, 64, 1024])
			for (const publishers of [1, 4])
				for (const readers of [1, 8]) {
					const tracks = Array.from({ length: publishers }, (_, i) => new Track.Producer(`p${i}`).accept({}));
					const sources: Group.Producer[] = [];
					const consumers: Group.Consumer[] = [];
					try {
						for (const track of tracks) {
							for (let sequence = 0; sequence < retained; sequence++) {
								const group = track.appendGroup();
								group.writeFrame({ payload });
								group.close();
							}
							const subscribers = Array.from({ length: readers }, () =>
								track.subscribe({ maxDelay: Milli(60_000) }),
							);
							// Drain replay through the normal ordered cursor while retaining its timeline.
							const ordered = subscribers.map((sub) => sub.ordered());
							for (let sequence = 0; sequence < retained; sequence++)
								for (const reader of ordered) {
									const group = await reader.nextGroup();
									if (group?.sequence !== sequence) throw new Error("history missing");
									await group.readFrame();
								}
							sources.push(track.appendGroup());
							for (const reader of ordered) {
								const group = await reader.nextGroup();
								if (!group) throw new Error("live group missing");
								consumers.push(group);
							}
						}
						const iterations = 2000;
						let start = 0;
						for (let iteration = 0; iteration < iterations + 20; iteration++) {
							if (iteration === 20) start = performance.now();
							const pending = consumers.map((consumer) => consumer.readFrame());
							for (const source of sources) source.writeFrame({ payload });
							for (const frame of await Promise.all(pending))
								if (frame?.payload[0] !== 7) throw new Error("frame missing");
						}
						const elapsed = performance.now() - start;
						console.log(
							`${retained},${publishers},${readers},${((elapsed * 1e6) / (iterations * publishers * readers)).toFixed(0)}`,
						);
					} finally {
						for (const track of tracks) track.close();
					}
				}
	} finally {
		installed.close();
	}
}
if (import.meta.main) await run();
