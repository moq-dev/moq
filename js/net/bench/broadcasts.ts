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
