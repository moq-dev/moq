/** Sweep publishers, readers, and retained history for a parked frame read's drift check. */
import { Manual, Milli } from "@moq/clock";
import { type Group, Time, Track } from "@moq/net";

/** Read live frames after a retained window, checking every delivered payload. */
export async function run(): Promise<void> {
	const clock = new Manual();
	const installed = clock.install();
	const payload = new Uint8Array([7]);
	const iterations = 2000;
	const warmup = 20;
	const offsets = [0, 80, 40];
	try {
		// The same reordered-PTS verdict runs here in Chromium before timing any work.
		const check = new Track.Producer("reordered").accept({ timescale: Time.Timescale.MILLI });
		const reader = check.subscribe({ maxDelay: Milli(80) });
		try {
			for (const timestamps of [[0], [100], [150, 200, 160]]) {
				const group = check.appendGroup();
				for (const ms of timestamps) group.writeFrame({ payload, timestamp: Time.Timestamp.fromMillis(ms) });
				group.finish();
			}
			if ((await reader.recvGroup())?.sequence !== 1) throw new Error("B-frame tail rewound the drift edge");
		} finally {
			reader.close();
			check.finish();
			check.close();
		}
		console.log("time_mode,retained,publishers,readers,ns_per_frame");
		for (const retained of [1, 64, 1024])
			for (const publishers of [1, 4])
				for (const readers of [1, 8])
					for (const mode of [0, 1, 2]) {
						const stamp = (ms: number, index: number) =>
							mode === 0
								? undefined
								: mode === 2 && index % 2 !== 0
									? Time.Timestamp.fromMicros(ms * 1000)
									: Time.Timestamp.fromMillis(ms);
						const tracks = Array.from({ length: publishers }, (_, i) =>
							new Track.Producer(`p${i}`).accept(mode !== 0 ? { timescale: Time.Timescale.MILLI } : {}),
						);
						const sources: Group.Producer[] = [];
						const consumers: Group.Consumer[] = [];
						try {
							for (const track of tracks) {
								for (let sequence = 0; sequence < retained; sequence++) {
									const group = track.appendGroup();
									group.writeFrame({
										payload,
										timestamp: stamp(sequence * 100, sequence),
									});
									group.finish();
								}
								const subscribers = Array.from({ length: readers }, () =>
									track.subscribe({
										maxDelay: Milli(
											(retained + Math.ceil((iterations + warmup) / offsets.length) + 1) * 100,
										),
									}),
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
							let start = 0;
							for (let iteration = 0; iteration < iterations + warmup; iteration++) {
								if (iteration === warmup) start = performance.now();
								const pending = consumers.map((consumer) => consumer.readFrame());
								const timestamp = stamp(
									retained * 100 +
										Math.floor(iteration / offsets.length) * 100 +
										offsets[iteration % offsets.length],
									iteration,
								);
								for (const source of sources) source.writeFrame({ payload, timestamp });
								for (const frame of await Promise.all(pending))
									if (
										frame?.payload[0] !== 7 ||
										frame.timestamp?.asMillis() !== timestamp?.asMillis()
									)
										throw new Error("frame missing or reordered");
							}
							const elapsed = performance.now() - start;
							console.log(
								`${mode},${retained},${publishers},${readers},${((elapsed * 1e6) / (iterations * publishers * readers)).toFixed(0)}`,
							);
						} finally {
							for (const track of tracks) track.finish();
							for (const track of tracks) track.close();
						}
					}
	} finally {
		installed.close();
	}
}
if (import.meta.main) await run();
