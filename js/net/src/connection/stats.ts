/**
 * Connection statistics: a point-in-time snapshot of the transport's counters.
 *
 * @module
 */
import * as Time from "../time.ts";

/**
 * A point-in-time snapshot of a connection's transport statistics.
 *
 * The counters come from `WebTransport.getStats()` (W3C field semantics) and
 * the estimates from the congestion controller and MoQ PROBE, mirroring the
 * shape of Rust's `moq_net::ConnectionStats`. Every field is optional: the
 * qmux/WebSocket fallback reports only the PROBE fields.
 *
 * @public
 */
export interface ConnectionStats {
	/** Smoothed round-trip time estimate in milliseconds. */
	rtt?: Time.Milli;

	/** Estimated send bandwidth from the congestion controller, in bits per second. */
	estimatedSendRate?: number;

	/** Estimated receive bandwidth from MoQ PROBE, in bits per second (moq-lite-03+ only). */
	estimatedRecvRate?: number;

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
 * Snapshot `quic` via `getStats()` into a {@link ConnectionStats}, or undefined
 * when the transport doesn't implement it (the qmux/WebSocket fallback, older
 * browsers). The caller fills the PROBE fields.
 *
 * @internal
 */
export async function transportStats(quic: WebTransport): Promise<ConnectionStats | undefined> {
	const getStats = (quic as { getStats?: () => Promise<TransportStats> }).getStats;
	if (typeof getStats !== "function") return undefined;
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
		return undefined;
	}
}
