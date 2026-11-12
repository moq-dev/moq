import { Duration } from "@moq/clock";
import type { Session } from "./session.ts";
import { transportOf } from "./transport.ts";
/**
 * Connection statistics: what the local transport counts, and what the peer reports.
 *
 * @module
 */

/**
 * A point-in-time snapshot of the transport's counters, from `WebTransport.getStats()`.
 *
 * Pulled on demand rather than pushed: the transport has no event to subscribe to, and
 * a caller computing a rate wants to choose its own sampling instants. Snapshot twice
 * and divide by the interval you measured.
 *
 * Every field is optional. The qmux/WebSocket fallback reports nothing; see
 * {@link Probe} for what the peer measures.
 *
 * The field names match Rust's `moq_net::session::Stats` but the counters keep each
 * stack's own semantics rather than being normalized. Notably W3C excludes
 * retransmissions and QUIC overhead from the byte counts where native QUIC includes
 * them, and counts packets where native QUIC counts datagrams.
 *
 * @public
 */
export interface Stats {
	/**
	 * Smoothed round-trip time estimate in milliseconds.
	 *
	 * Usually absent: browsers don't report an RTT yet even though the W3C dictionary
	 * has the field. Use {@link Probe.rtt}, which the peer measures.
	 */
	rtt?: Duration;

	/** Estimated send bandwidth from the congestion controller, in bits per second. */
	estimatedSendRate?: number;

	/** Total bytes sent on streams and datagrams, excluding retransmissions and QUIC overhead. */
	bytesSent?: number;

	/** Total bytes received on streams and datagrams, including duplicates, excluding QUIC overhead. */
	bytesReceived?: number;

	/** Bytes currently declared lost. The estimate can decrease when a loss proves spurious. */
	bytesLost?: number;

	/** Total packets sent, including retransmissions. */
	packetsSent?: number;

	/** Total packets received, including duplicates. */
	packetsReceived?: number;

	/** Packets currently declared lost. The estimate can decrease when a loss proves spurious. */
	packetsLost?: number;

	/**
	 * Datagrams received for a subscription with no datagram track: one this session doesn't
	 * know, has ended, or whose answer it hasn't received. Absent on a version without datagrams:
	 * moq-transport and moq-lite before 05.
	 */
	datagramsUnrouted?: number;
}

/**
 * Estimates measured by the peer and delivered over the MoQ PROBE stream.
 *
 * Pushed rather than pulled: these arrive when the peer sends them, so they're read
 * from a signal. Empty until the first PROBE message, on versions without PROBE
 * (moq-lite-01/02, moq-transport), and again once the stream ends, so a consumer
 * never holds a stale estimate from a dead stream.
 *
 * @public
 */
export interface Probe {
	/**
	 * Round-trip time in milliseconds.
	 *
	 * The only RTT available today, since browsers don't populate the transport's.
	 * Measured through the peer's application layer, so it reads higher than a
	 * transport-level RTT would.
	 */
	rtt?: Duration;

	/** The peer's send-rate estimate toward us, so our receive rate, in bits per second. */
	estimatedRecvRate?: number;
}

/**
 * The subset of the W3C `WebTransportConnectionStats` dictionary we consume.
 * `getStats()` is not in TypeScript's DOM lib yet, so it is declared here.
 *
 * @internal
 */
export interface TransportStats {
	smoothedRtt?: number;
	estimatedSendRate?: number | null;
	bytesSent?: number;
	bytesReceived?: number;
	bytesLost?: number;
	packetsSent?: number;
	packetsReceived?: number;
	packetsLost?: number;
}

// Transports whose `getStats()` is a stub that measures nothing.
const stubs = new WeakSet<WebTransport>();

/**
 * Decide at setup whether `quic`'s `getStats()` measures anything. Safari's is a stub
 * whose `smoothedRtt` and counters are always 0 and whose `estimatedSendRate` is null;
 * QUIC's smoothed RTT starts at 333 ms, so a 0 RTT with no rate means nothing was
 * measured: the transport is then treated as having no `getStats()` at all. A rate
 * alongside a 0 RTT keeps the transport usable. Only this setup-time call decides, so a real RTT from a
 * later WebKit lifts the rule.
 *
 * @internal
 */
export async function inspect(quic: WebTransport): Promise<void> {
	const getStats = (quic as { getStats?: () => Promise<TransportStats> }).getStats;
	if (typeof getStats !== "function") return;
	try {
		const stats = await getStats.call(quic);
		if (stats.smoothedRtt === 0 && stats.estimatedSendRate == null) stubs.add(quic);
	} catch {
		// A transport that can't answer now may later; `transportStats` handles the rejection.
	}
}

/**
 * `quic`'s `getStats()`, or `undefined` when it has none or {@link inspect} found a stub.
 *
 * @internal
 */
export function statsOf(quic: WebTransport): (() => Promise<TransportStats>) | undefined {
	const getStats = (quic as { getStats?: () => Promise<TransportStats> }).getStats;
	if (typeof getStats !== "function" || stubs.has(quic)) return undefined;
	return () => getStats.call(quic);
}

/**
 * Snapshot `quic` via `getStats()`, or an empty snapshot when the transport doesn't
 * implement it (older browsers), it is a stub, or the call fails.
 *
 * @internal
 */
export async function transportStats(quic: WebTransport): Promise<Stats> {
	const getStats = statsOf(quic);
	if (!getStats) return {};
	try {
		const stats = await getStats();
		return {
			rtt: stats.smoothedRtt !== undefined ? Duration.millis(stats.smoothedRtt) : undefined,
			estimatedSendRate: stats.estimatedSendRate ?? undefined,
			bytesSent: stats.bytesSent,
			bytesReceived: stats.bytesReceived,
			bytesLost: stats.bytesLost,
			packetsSent: stats.packetsSent,
			packetsReceived: stats.packetsReceived,
			packetsLost: stats.packetsLost,
		};
	} catch {
		return {};
	}
}

/**
 * Whether `quic` can report a send-rate estimate: only a WebTransport session with a
 * real `getStats()`. The WebSocket fallback's qmux session reports no counters, the WebRTC
 * data channel's throws, shipping Chromium implements none, and Safari's is a stub.
 *
 * @internal
 */
export function canEstimate(quic: WebTransport): boolean {
	return transportOf(quic) === "webtransport" && statsOf(quic) !== undefined;
}

// The sessions whose transport can report a send-rate estimate.
const estimating = new WeakSet<Session>();

/** Record whether `session`'s transport `quic` can report a send-rate estimate. @internal */
export function detect(session: Session, quic: WebTransport): void {
	if (canEstimate(quic)) estimating.add(session);
}

/**
 * Whether `session` can report a send-rate estimate, so sampling it is worth a timer.
 *
 * @internal
 */
export function estimates(session: Session): boolean {
	return estimating.has(session);
}
