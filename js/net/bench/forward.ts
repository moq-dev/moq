/**
 * Sweep the session forwarder: a route re-price over interest heads and received routes, and
 * a watch coming and going over held watches and attached sessions.
 */

import { Manual } from "@moq/clock";
import { Once } from "@moq/signals";
import { announcedView } from "../src/announced.test-util";
import * as Announce from "../src/announced.ts";
import { AuthSession } from "../src/auth_session.ts";
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
// Keep discovery's linger fixed while measuring with the host's performance clock.
new Manual().install();

// Let every interest stream's forwarding loop drain its queue.
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

/** A session standing in for the wire: one announce stream per requested prefix, driven by the bench. */
class FakeSession {
	readonly streams: Announce.Producer[] = [];
	readonly closed: Promise<Error | null>;
	die!: () => void;

	constructor() {
		registerWire(this, {
			consume: () => new BroadcastProducer().consume(),
			goaway: new Once(),
			exclusive: false,
			auth: new AuthSession({ peerGrant: { publish: new Path.Patterns(), subscribe: new Path.Patterns() } }),
		});
		this.closed = new Promise((resolve) => {
			this.die = () => resolve(null);
		});
	}

	announced(): Announce.Consumer {
		const stream = new Announce.Producer();
		this.streams.push(stream);
		// This peer has no initial routes; later announcements are live updates.
		stream.synced();
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

			// Watching everything narrows to one request per head.
			const table = await announcedView(origin);
			if (session.streams.length !== headCount) throw new Error("expected one announce stream per head");

			// Routes beneath the heads, spread across them, plus one above every head. The wire
			// presents that covering route on each head's stream; it lands once, driven by the first.
			for (let index = 0; index < routeCount; index++) {
				const head = index % headCount;
				announce(session.streams[head], Path.join(heads[head], Path.from(`r${index}`)), 1n, "start");
			}
			for (const stream of session.streams) announce(stream, Path.empty(), 1n, "start");
			await flush();

			const path = touched === "covering" ? Path.empty() : Path.join(heads[0], Path.from("r0"));
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
				if (table.peek().get(path)?.cost !== cost) throw new Error("re-price did not land");
				checksum += table.peek().size;
			}
			const elapsed = performance.now() - start;
			console.log(`${touched},${headCount},${routeCount},${((elapsed * 1000) / updates).toFixed(1)}`);

			await table.close();
			session.die();
			await flush();
			origin.close();
		}
	}
}

// One watch opening and closing reaches every attached session, but only as the change to its
// own prefix. Held watches and sessions are the two fan-out axes. The sweep spans 256x in held
// watches, so a cost that rescans them lands well past this loose margin on a noisy machine.
const maxSlope = 5;
const baselines = new Map<number, number>();
console.log("watchers,sessions,watch_us");
for (const watchers of [8, 128, 2048]) {
	for (const sessions of [1, 8, 32]) {
		const origin = new Producer();
		const attached = Array.from({ length: sessions }, () => {
			const session = new FakeSession();
			forwardAnnounced(session as unknown as Established, origin);
			return session;
		});
		const held = Array.from({ length: watchers }, (_, index) =>
			origin.announced(Path.Pattern.subtree(Path.from(`w${index}`))),
		);
		await flush();

		const samples: number[] = [];
		for (let round = 0; round < 6; round++) {
			const start = performance.now();
			for (let index = 0; index < 64; index++) {
				const watch = origin.announced(Path.Pattern.subtree(Path.from(`touched/${index}`)));
				await flush();
				watch.close();
				await flush();
			}
			if (round > 0) samples.push(((performance.now() - start) * 1000) / 64);
		}
		samples.sort((a, b) => a - b);
		const opened = attached[0]?.streams.length ?? 0;
		// Each touched prefix opens once; the linger keeps it for the next round.
		if (opened !== watchers + 64) throw new Error(`expected ${watchers + 64} requests, saw ${opened}`);
		checksum += opened;
		const us = samples[2];
		console.log(`${watchers},${sessions},${us.toFixed(2)}`);
		const baseline = baselines.get(sessions) ?? us;
		baselines.set(sessions, baseline);
		if (us > baseline * maxSlope) {
			throw new Error(
				`${watchers} held watches cost ${us.toFixed(2)} us, over ${maxSlope}x ${baseline.toFixed(2)}`,
			);
		}

		for (const watch of held) watch.close();
		for (const session of attached) session.die();
		await flush();
		origin.close();
	}
}
if (checksum === 0) throw new Error("benchmark did no work");
