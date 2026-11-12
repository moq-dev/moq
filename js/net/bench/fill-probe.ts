/** Sweep a reader's fill probes over the received stale runs it cannot trust, and the readers. */
import { Duration } from "@moq/clock";
import { Producer as BroadcastProducer } from "../src/broadcast.ts";
import { hooks, staleHole } from "../src/internal.ts";

console.debug = () => {};
console.log("runs,readers,probe_us");
for (const runs of [250, 1000, 4000, 16000]) {
	for (const readers of [1, 8]) {
		const broadcast = new BroadcastProducer();
		const track = broadcast.createTrack("video", { maxAge: null });
		const live = track.createGroup(2n);
		live.writeFrame({ payload: new Uint8Array([0]) });
		live.finish();
		const requests = track.requests();
		let asked = 0;
		const serving = (async () => {
			for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) asked++;
		})();
		const subs = Array.from({ length: readers }, () =>
			track.subscribe({
				// Capped, so the upstream's verdicts, judged under an open range, settle none of them.
				ranges: [{ start: { absolute: 2n }, end: { absolute: BigInt(8 + 2 * runs) } }],
				delay: { max: Duration.millis(500) },
			}),
		);
		for (const sub of subs) {
			sub.tryRecvGroup();
			sub.tryRecvGroup();
		}
		const start = performance.now();
		// One run per drop, each probed by every reader before the next: the upstream judged them
		// under a wider budget, so no reader trusts them and each is asked of a fill.
		for (let i = 0; i < runs; i++) {
			const sequence = BigInt(4 + 2 * i);
			hooks.dropTrackGroups(track, sequence, sequence + 1n, staleHole(60_000));
			for (const sub of subs) sub.tryRecvGroup();
		}
		const elapsed = performance.now() - start;
		// The handler takes the queued fills once this turn yields. The readers join one per run.
		await new Promise((resolve) => setTimeout(resolve, 0));
		for (const sub of subs) sub.close();
		requests.close();
		await serving;
		if (asked !== runs) throw new Error(`asked ${asked} fills, expected ${runs}`);
		console.log(`${runs},${readers},${((elapsed * 1000) / (runs * readers)).toFixed(2)}`);
		broadcast.close();
	}
}
