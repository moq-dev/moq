/** Sweep interest heads and received routes for one route re-price forwarded from a session. */
import * as Announce from "../src/announced.ts";
import { Producer as BroadcastProducer } from "../src/broadcast.ts";
import type { Established } from "../src/connection/established.ts";
import { forwardAnnounced } from "../src/connection/forward.ts";
import { Route } from "../src/hop.ts";
import { Producer } from "../src/origin.ts";
import * as Path from "../src/path.ts";
import { registerWire } from "../src/wire.ts";

const headCounts = [1, 4, 16];
const routeCounts = [8, 32, 128];
const updates = 32;
let checksum = 0;

// Let every interest stream's forwarding loop drain its queue.
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

/** A session standing in for the wire: one announce stream per interest head, driven by the bench. */
class FakeSession {
	readonly discovery = true;
	readonly streams: Announce.Producer[] = [];
	readonly closed: Promise<Error | null>;
	die!: () => void;

	constructor() {
		registerWire(this, { consume: () => new BroadcastProducer().consume() });
		this.closed = new Promise((resolve) => {
			this.die = () => resolve(null);
		});
	}

	announced(): Announce.Consumer {
		const stream = new Announce.Producer();
		this.streams.push(stream);
		return stream.consume();
	}
}

const announce = (stream: Announce.Producer, prefix: Path.Valid, cost: bigint, kind: "start" | "update") =>
	stream.append({ prefix, captures: undefined, kind, route: Route.normalize({ cost }) });

console.log("touched,heads,routes,update_us");
for (const touched of ["covering", "single"] as const) {
	for (const headCount of headCounts) {
		for (const routeCount of routeCounts) {
			const origin = new Producer();
			const heads = Array.from({ length: headCount }, (_, index) => Path.from(`h${index}`));
			const scoped = origin.scope(
				Path.empty(),
				new Path.Patterns(heads.map((head) => Path.Pattern.subtree(head))),
			);
			const session = new FakeSession();
			forwardAnnounced(session as unknown as Established, scoped);
			if (session.streams.length !== headCount) throw new Error("expected one announce stream per head");

			// Routes beneath the heads, spread across them, plus one above every head. The wire
			// presents that covering route on each head's stream, so it lands once per head.
			for (let index = 0; index < routeCount; index++) {
				const head = index % headCount;
				announce(session.streams[head], Path.join(heads[head], Path.from(`r${index}`)), 1n, "start");
			}
			for (const stream of session.streams) announce(stream, Path.empty(), 1n, "start");
			await flush();

			const path = touched === "covering" ? Path.empty() : Path.join(heads[0], Path.from("r0"));
			const table = origin.broadcasts();
			if (table.peek().size !== routeCount + 1) throw new Error(`expected ${routeCount + 1} routes`);

			const start = performance.now();
			for (let index = 0; index < updates; index++) {
				const cost = BigInt(index + 2);
				if (touched === "covering") {
					for (const stream of session.streams) announce(stream, path, cost, "update");
				} else {
					announce(session.streams[0], path, cost, "update");
				}
				await flush();
				if (table.peek().get(path)?.cost.warm !== cost) throw new Error("re-price did not land");
				checksum += table.peek().size;
			}
			const elapsed = performance.now() - start;
			console.log(`${touched},${headCount},${routeCount},${((elapsed * 1000) / updates).toFixed(1)}`);

			session.die();
			await flush();
			origin.close();
		}
	}
}
if (checksum === 0) throw new Error("benchmark did no work");
