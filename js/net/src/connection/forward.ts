/**
 * Feeds a session's announced broadcasts into an origin; the `subscribe` connect option.
 *
 * @module
 */
import type { Dispose } from "@moq/signals";
import type { Producer as OriginProducer } from "../origin.ts";
import type * as Path from "../path.ts";
import type { Established } from "./established.ts";

/**
 * Wire a session into `origin` for the session's lifetime: forward the peer's announced
 * broadcasts into the table, and answer the origin's open requests with blind
 * subscriptions.
 *
 * Each active announcement inserts a lazily-subscribing front, so nothing touches the wire
 * until somebody consumes the path. A retraction removes the entry, and so does the session
 * dying (the announce stream ends with it), which is what scopes remote entries to the
 * session that discovered them. A reconnect wires a fresh session to the same origin and
 * re-populates it.
 *
 * Without discovery there are no announcements to forward, so the table stays empty and
 * requests are the only way through; see the origin's `request`.
 *
 * @internal
 */
export function forwardAnnounced(conn: Established, origin: OriginProducer): void {
	const detach = origin.attach(conn.discovery);
	void conn.closed.then(detach);

	void serveRequests(conn, origin);

	if (!conn.discovery) {
		console.warn("relay does not support broadcast discovery; broadcasts resolve on request only.");
		return;
	}

	const announced = conn.announced();
	const inserted = new Map<Path.Valid, Dispose>();

	// End the stream the moment the session closes rather than waiting for the wire to
	// error it, so the retractions below land promptly.
	void conn.closed.then(() => announced.close());

	void (async () => {
		try {
			for (;;) {
				const event = await announced.next();
				if (!event) break;

				if (event.active) {
					// A same-path re-announce supersedes: retract the old front first.
					inserted.get(event.path)?.();
					inserted.set(event.path, origin.insertRemote(event.path, conn.consume(event.path)));
				} else {
					const dispose = inserted.get(event.path);
					inserted.delete(event.path);
					dispose?.();
				}
			}
		} catch {
			// The session died mid-stream; the cleanup below retracts everything it fed.
		} finally {
			for (const dispose of inserted.values()) dispose();
			inserted.clear();
			announced.close();
		}
	})();
}

/**
 * Answer the origin's open requests with blind subscriptions for the session's lifetime.
 *
 * Every session answers, discovery or not: subscribing to an unannounced path is always
 * legal, and a missing broadcast surfaces as a reset on the first track. The first session
 * to answer wins; when this session dies its answers are withdrawn so a later session
 * answers again, which is what makes a request span reconnects.
 */
async function serveRequests(conn: Established, origin: OriginProducer): Promise<void> {
	// The withdraws for the answers this session provided, so a dead session only takes
	// back its own.
	const answered = new Map<Path.Valid, Dispose>();

	let dead = false;
	const closed = conn.closed.then(() => {
		dead = true;
	});

	const requests = origin.requests;
	for (;;) {
		const map = requests.peek();
		if (!map || dead) break;

		for (const [path, slot] of map) {
			if (answered.has(path) || slot.front.peek() !== undefined) continue;
			const withdraw = origin.answer(path, conn.consume(path));
			if (withdraw) answered.set(path, withdraw);
		}

		// A withdrawn request already released the answer; just forget our claim on the path.
		for (const [path, withdraw] of [...answered]) {
			if (map.has(path)) continue;
			answered.delete(path);
			withdraw();
		}

		await Promise.race([requests.changed(), closed]);
	}

	// Session gone: withdraw our answers, waking a standby session to provide fresh ones.
	for (const withdraw of answered.values()) {
		withdraw();
	}
	answered.clear();
}
