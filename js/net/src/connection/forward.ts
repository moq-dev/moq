/**
 * Feeds a session's announced broadcasts into an origin; the `subscribe` connect option.
 *
 * @module
 */
import type { Dispose } from "@moq/signals";
import type * as announce from "../announced.ts";
import type { Producer as OriginProducer, RequestSlot } from "../origin.ts";
import * as Path from "../path.ts";
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
 * Announcements are solicited lazily and per prefix: nothing is asked for until something
 * reads the origin's announcements, so an app that only publishes, or only consumes known
 * paths via requests, costs the peer no announce traffic at all. Requests deliberately do
 * not count as interest: a session answers them with blind subscriptions, which need no
 * announcement.
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

	void solicitAnnounced(conn, origin, {
		isDead: () => dead,
		// Discovery ended while the session lives, and nothing reopens the stream on this
		// connection. Downgrade the attachment rather than leaving the origin claiming a
		// discovery that no longer works: announcement-gated consumers would wait forever on a
		// table this session can no longer fill. Now they fall back to standing requests,
		// which this session still answers.
		onDiscoveryLost: (failure) => {
			console.warn("broadcast discovery failed; broadcasts resolve on request only.", failure);
			detach();
			detach = origin.attach(false);
		},
	});
}

/** What {@link solicitAnnounced} needs from the session wiring around it. */
interface SolicitHooks {
	/** Whether the session is already gone, so a stream ending is expected rather than a failure. */
	isDead(): boolean;
	/** Called once, when discovery fails under a live session. */
	onDiscoveryLost(failure: unknown): void;
}

/**
 * Open one wire announcement stream per interested prefix, forwarding what arrives into the
 * origin's remote table.
 *
 * A stream is opened for any interested prefix no open stream already covers, and is then
 * held for the session's lifetime rather than closed when the last interested consumer
 * leaves. Streams are what carry the routes: closing one retracts the entries it fed, so a
 * consumer still holding a broadcast at that path would see it go offline and come back as a
 * blind answer. Closing on idle needs the table to know which entries are still being read,
 * which it does not, so the saving here is not asking in the first place.
 *
 * Overlapping streams (a narrow prefix, then a broader one) are deduped per path, so the
 * table holds one front per path however many streams announced it. Without that a
 * retraction from one stream would swap the front the other still provides, which reads as a
 * republish and makes consumers re-subscribe for nothing.
 */
async function solicitAnnounced(conn: Established, origin: OriginProducer, hooks: SolicitHooks): Promise<void> {
	// The paths this session put in the table, refcounted across the streams announcing them.
	const inserted = new Map<Path.Valid, { count: number; dispose: Dispose }>();
	// Prefixes with a live stream, and the streams themselves. Only grows: streams are
	// sticky (see above), and every one is closed together on teardown.
	const open: Path.Valid[] = [];
	const streams: announce.Consumer[] = [];
	let stopped = false;

	// Settles when the first stream ends, however it ends, carrying the failure if there was
	// one. A stream that ends means the peer stopped answering, which is what the caller
	// reports as discovery lost; with the usual single root stream this is exactly the
	// behavior of the eager forwarder it replaced.
	let endStream!: (failure: unknown) => void;
	const streamEnded = new Promise<unknown>((resolve) => {
		endStream = resolve;
	});

	// One stream's read loop, feeding the shared table entries above.
	const pump = async (prefix: Path.Valid): Promise<void> => {
		const announced = conn.announced(prefix);
		streams.push(announced);
		// End the stream the moment the session closes rather than waiting for the wire to
		// error it, so the retractions below land promptly.
		void conn.closed.then(() => announced.close());
		if (stopped) announced.close();

		try {
			for (;;) {
				const event = await announced.next();
				if (!event || stopped) break;

				// Paths arrive relative to the stream's prefix; the table is absolute.
				const path = Path.join(prefix, event.path);
				const existing = inserted.get(path);

				if (event.active) {
					// A re-announce supersedes, so replace the front; a second stream covering
					// the same path just shares the entry. Either way the table sees one front,
					// so no consumer re-subscribes for a path that never went away.
					existing?.dispose();
					const dispose = origin.insertRemote(path, conn.consume(path));
					if (existing) {
						existing.dispose = dispose;
						existing.count += 1;
					} else {
						inserted.set(path, { count: 1, dispose });
					}
				} else if (existing) {
					existing.count -= 1;
					if (existing.count === 0) {
						inserted.delete(path);
						existing.dispose();
					}
				}
			}
			endStream(undefined);
		} catch (err) {
			endStream(err);
		} finally {
			announced.close();
		}
	};

	let lost: { failure: unknown } | undefined;
	for (;;) {
		const interest = origin.interest.peek();
		// The origin closed, or the session died: either way stop soliciting. Neither is a
		// discovery failure, so the attachment is left alone.
		if (!interest || hooks.isDead()) break;

		for (const prefix of interest.keys()) {
			// A stream for a shorter prefix already carries this subtree.
			if (open.some((covering) => Path.stripPrefix(covering, prefix) !== null)) continue;
			open.push(prefix);
			void pump(prefix);
		}

		// Woken by new interest, by the session dying, or by a stream ending.
		const woken = await Promise.race([
			origin.interestChanged().then(() => undefined),
			conn.closed.then(() => undefined),
			streamEnded.then((failure) => ({ failure })),
		]);
		if (woken) {
			lost = woken;
			break;
		}
	}

	// Stop the streams still running before releasing what they fed, so a pump cannot insert
	// into a table this session no longer claims anything in.
	stopped = true;
	for (const stream of streams) stream.close();
	for (const { dispose } of inserted.values()) dispose();
	inserted.clear();

	if (lost && !hooks.isDead()) {
		hooks.onDiscoveryLost(lost.failure);
	}
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
			if (answered.get(path)?.slot === slot || slot.front.peek() !== undefined) continue;
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
