import type { Duration } from "@moq/clock";
import { type Getter, Signal } from "@moq/signals";

/**
 * How long a session may go without hearing from its peer: `stale` after `soft`, closed after
 * `hard`. Mirrors Rust's `moq_net::session::KeepAlive`.
 *
 * The transport probes a peer that stays quiet for half of `soft`, and anything the peer sends
 * counts as hearing it, so a busy session pays nothing. Only the WebSocket fallback on qmux-01
 * and newer can probe; WebTransport has no ping, so a connect with a keep-alive dials the
 * WebSocket alone, and a supplied WebTransport refuses it. A `hard` longer than the negotiated
 * idle timeout is refused too, since the idle timeout stays in force as a backstop.
 */
export interface KeepAlive {
	/** How long the peer may go unheard before the session turns `stale`. */
	soft: Duration;
	/** How long the peer may go unheard before the session closes. */
	hard: Duration;
}

/**
 * Whether a session heard from its peer within its {@link KeepAlive}'s `soft` limit. Evidence
 * about the whole session, never one track: a quiet track on a live session stays `live`.
 */
export type Liveness = "live" | "stale";

/** The part of a qmux session that runs a keep-alive, read without loading `@moq/qmux`. */
interface Probing {
	keepAlive(config: KeepAlive): void;
	readonly stale: boolean;
	staleChanged(): Promise<void>;
	readonly closed: Promise<unknown>;
}

/**
 * Start `config` on `quic`, returning the peer's liveness.
 *
 * @throws On a transport that cannot probe its peer, as WebTransport cannot, or values the
 * transport refuses.
 */
export function start(quic: WebTransport, config: KeepAlive): Getter<Liveness> {
	if (!("keepAlive" in quic)) {
		throw new Error("keep-alive needs a transport that can probe its peer, which WebTransport cannot");
	}
	const probing = quic as unknown as Probing;
	probing.keepAlive(config);

	const liveness = new Signal<Liveness>("live");
	let closed = false;
	const end = () => {
		closed = true;
	};
	probing.closed.then(end, end);
	void (async () => {
		// The session settles `closed` before its last `staleChanged`, so this ends with it.
		while (!closed) {
			await probing.staleChanged();
			liveness.set(probing.stale ? "stale" : "live");
		}
	})();
	return liveness;
}
