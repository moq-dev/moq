/**
 * Feeds a session's announced broadcasts into an origin; the `subscribe` connect option.
 *
 * @module
 */
import type { Dispose } from "@moq/signals";
import type { Producer as OriginProducer, RequestSlot } from "../origin.ts";
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
	// Reassigned if discovery dies under a live session, so the origin stops counting this
	// one as a discovering session. Called through a closure so the session's death always
	// detaches whichever attachment is current.
	let detach = origin.attach(conn.discovery);

	let dead = false;
	void conn.closed.then(() => {
		dead = true;
		detach();
	});

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
		let failure: unknown;
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
		} catch (err) {
			// The session died mid-stream, or the relay refused or reset the stream. The
			// cleanup below retracts everything this stream fed either way.
			failure = err;
		} finally {
			for (const dispose of inserted.values()) dispose();
			inserted.clear();
			announced.close();

			// Discovery ended while the session lives, and nothing reopens the stream on this
			// connection. Downgrade the attachment rather than leaving the origin claiming a
			// discovery that no longer works: announcement-gated consumers would wait forever
			// on a table this session can no longer fill. Now they fall back to standing
			// requests, which this session still answers.
			if (!dead) {
				console.warn("broadcast discovery failed; broadcasts resolve on request only.", failure);
				detach();
				detach = origin.attach(false);
			}
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
 *
 * A path the table already routes is left alone. A request resolves to the table's route over
 * any blind answer, so answering one would only park a handle nothing reads.
 */
async function serveRequests(conn: Established, origin: OriginProducer): Promise<void> {
	// The withdraws for the answers this session provided, so a dead session only takes
	// back its own. Keyed by path but remembering the slot, because a path outlives its
	// slot: the last handle closing tears the slot down and a new request installs a fresh
	// one, and those two writes coalesce into a single wakeup. Matching on the path alone
	// would read the new slot as already answered and leave it unanswered forever.
	const answered = new Map<Path.Valid, { slot: RequestSlot; withdraw: Dispose }>();

	let dead = false;
	const closed = conn.closed.then(() => {
		dead = true;
	});

	for (;;) {
		const map = origin.requests.peek();
		if (!map || dead) break;

		for (const [path, slot] of map) {
			if (answered.get(path)?.slot === slot || slot.answer !== undefined) continue;
			if (origin.routes(path)) continue;
			const withdraw = origin.answer(path, conn.consume(path));
			if (withdraw) answered.set(path, { slot, withdraw });
		}

		// A withdrawn request already released the answer; just forget our claim on the path.
		// A replaced slot counts as withdrawn: the answer we hold belongs to the slot that
		// went away, not to whatever now occupies the path.
		for (const [path, entry] of [...answered]) {
			if (map.get(path) === entry.slot) continue;
			answered.delete(path);
			entry.withdraw();
		}

		// Woken by the table too, not just the requests: a path that stops being routed needs
		// the blind answer this loop skipped while it was.
		await Promise.race([origin.changed(), closed]);
	}

	// Session gone: withdraw our answers, waking a standby session to provide fresh ones.
	for (const { withdraw } of answered.values()) {
		withdraw();
	}
	answered.clear();
}
