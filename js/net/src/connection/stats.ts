/**
 * Connection statistics: what the local transport counts, and what the peer reports.
 *
 * @module
 */
import * as Time from "../time.ts";

/**
 * A point-in-time snapshot of the transport's counters, from `WebTransport.getStats()`.
 *
 * Pulled on demand rather than pushed: the transport has no event to subscribe to, and
 * a caller computing a rate wants to choose its own sampling instants. Snapshot twice
 * and divide by the interval you measured.
 *
 * Every field is optional. The qmux/WebSocket fallback implements no `getStats()` at
 * all and reports nothing; see {@link Probe} for what the peer measures.
 *
 * The field names match Rust's `moq_net::ConnectionStats` but the counters keep each
 * stack's own semantics rather than being normalized. Notably W3C excludes
 * retransmissions and QUIC overhead from the byte counts where quinn includes them,
 * and counts packets where quinn counts datagrams.
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
	rtt?: Time.Milli;

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
	rtt?: Time.Milli;

	/** The peer's estimate of the bitrate it is receiving from us, in bits per second. */
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

/**
 * Snapshot `quic` via `getStats()`, or an empty snapshot when the transport doesn't
 * implement it (the qmux/WebSocket fallback, older browsers) or the call fails.
 *
 * @internal
 */
export async function transportStats(quic: WebTransport): Promise<Stats> {
	const getStats = (quic as { getStats?: () => Promise<TransportStats> }).getStats;
	if (typeof getStats !== "function") return {};
	try {
		const stats = await getStats.call(quic);
		return {
			rtt: stats.smoothedRtt !== undefined ? Time.Milli(stats.smoothedRtt) : undefined,
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
