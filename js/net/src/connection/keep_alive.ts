import type { Duration } from "@moq/clock";
import { type Getter, Signal } from "@moq/signals";
import { isWebTransportSupported } from "./browser.ts";

/**
 * How long a session may go without hearing from its peer: `stale` after `soft`, closed after
 * `hard`. Mirrors Rust's `moq_net::transport::KeepAlive`.
 *
 * The transport probes a peer that stays quiet for half of `soft`, and anything the peer sends
 * counts as hearing it, so a busy session pays nothing. Only qmux-01 and newer (the WebSocket
 * transport) can probe: WebTransport has no ping, so a connect that would use it refuses the
 * keep-alive, as does a supplied WebTransport. A `hard` longer than the negotiated idle timeout
 * is refused too, since the idle timeout stays in force as a backstop.
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
 * Refuse a keep-alive on a dial that would use WebTransport, before anything opens. Only a
 * supplied `transport` skips the race, and {@link start} checks that one.
 */
export function check(props: { keepAlive?: KeepAlive; transport?: unknown }): void {
	if (props.keepAlive && !props.transport && isWebTransportSupported()) {
		throw new TypeError(
			"keepAlive cannot run here: this browser would dial WebTransport, which has no ping to probe its peer",
		);
	}
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
