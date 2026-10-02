import { Producer as BroadcastProducer } from "../src/broadcast.ts";
/** Sweep route and observer counts for a single touched-path route update. */
import { Producer } from "../src/origin.ts";
import * as Path from "../src/path.ts";

const routeCounts = [8, 32, 128];
const observerCounts = [1, 8, 32];
const updates = 32;
let checksum = 0;

console.log("scope,routes,observers,update_us");
for (const scope of ["unscoped", "distinct-scopes", "producer-scopes", "rooted-producers"] as const) {
	for (const routeCount of routeCounts) {
		for (const observerCount of observerCounts) {
			const origin = new Producer();
			const handles = Array.from({ length: routeCount }, (_, index) =>
				origin.dynamic(Path.from(`room/${index}`)),
			);
			const touched = Path.from(scope === "rooted-producers" ? "0" : "room/0");
			let notifications = 0;
			const disposes = Array.from({ length: observerCount }, (_, index) =>
				(scope === "producer-scopes"
					? origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.parse(`room/**/tag-${index}`)]))
					: scope === "rooted-producers"
						? origin.scope(Path.from("room"), new Path.Patterns([Path.Pattern.parse(`**/tag-${index}`)]))
						: origin
				)
					.broadcasts(scope === "distinct-scopes" ? Path.Pattern.parse(`room/**/tag-${index}`) : undefined)
					.subscribe((routes) => {
						if (routes.get(touched) === undefined) throw new Error("touched route disappeared");
						notifications++;
						checksum += routes.size;
					}),
			);

			const start = performance.now();
			for (let index = 0; index < updates; index++) {
				handles[0].update({ cost: BigInt(index + 1) });
				await Promise.resolve(); // Flush the route signal and every observing getter.
			}
			const elapsed = performance.now() - start;
			if (notifications !== updates * observerCount) {
				throw new Error(`expected ${updates * observerCount} notifications, got ${notifications}`);
			}
			console.log(`${scope},${routeCount},${observerCount},${((elapsed * 1000) / updates).toFixed(1)}`);
			for (const dispose of disposes) dispose();
			for (const handle of handles) handle.close();
			origin.close();
		}
	}
}
if (checksum === 0) throw new Error("benchmark did no work");

// Sweep both axes while only one track's demand changes.
const demandUpdates = 1024;
console.log("tracks,observers,demand_update_us");
for (const trackCount of [8, 32, 128]) {
	for (const observerCount of [1, 8, 32]) {
		const broadcast = new BroadcastProducer();
		const tracks = Array.from({ length: trackCount }, (_, index) => broadcast.createTrack(`track-${index}`));
		const demand = broadcast.demand();
		let notifications = 0;
		const disposes = Array.from({ length: observerCount }, () =>
			demand.used.subscribe(() => {
				notifications++;
			}),
		);
		const start = performance.now();
		for (let index = 0; index < demandUpdates; index++) {
			const subscriber = tracks[0].subscribe();
			await Promise.resolve();
			await Promise.resolve();
			if (!demand.used.peek()) throw new Error("missing demand");
			subscriber.close();
			await demand.unused();
			await Promise.resolve();
		}
		if (notifications !== demandUpdates * 2 * observerCount) throw new Error(`lost demand edge: ${notifications}`);
		console.log(
			`${trackCount},${observerCount},${(((performance.now() - start) * 1000) / demandUpdates).toFixed(1)}`,
		);
		for (const dispose of disposes) dispose();
		for (const track of tracks) track.close();
		broadcast.close();
	}
}
