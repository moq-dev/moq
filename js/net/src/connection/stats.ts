/**
 * Connection statistics: a live view of the transport's counters.
 *
 * @module
 */
import { type Getter, Signal } from "@moq/signals";
import * as Time from "../time.ts";

/** How often the transport is polled for fresh counters. */
const POLL_INTERVAL = 100; // ms

/**
 * A point-in-time snapshot of a connection's transport statistics.
 *
 * The counters come from `WebTransport.getStats()` (W3C field semantics) and
 * the estimates from the congestion controller and MoQ PROBE, mirroring the
 * shape of Rust's `moq_net::ConnectionStats`. Every field is optional: the
 * qmux/WebSocket fallback reports only the PROBE fields.
 *
 * The field names match Rust but the counters are whatever the underlying stack
 * reports, so they are not normalized across the two. Notably W3C excludes
 * retransmissions and QUIC overhead from the byte counts where quinn includes
 * them, and counts packets where quinn counts datagrams.
 *
 * @public
 */
export interface Stats {
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
 * Snapshot `quic` via `getStats()` into a {@link Stats}, or undefined
 * when the transport doesn't implement it (the qmux/WebSocket fallback, older
 * browsers). The caller fills the PROBE fields.
 *
 * @internal
 */
export async function transportStats(quic: WebTransport): Promise<Stats | undefined> {
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

/**
 * What MoQ PROBE contributes to a snapshot, measured by the peer's application layer
 * rather than the transport.
 *
 * @internal
 */
export interface ProbeStats {
	rtt?: Time.Milli;
	estimatedRecvRate?: number;
}

/**
 * Merge PROBE's estimates into the transport's counters.
 *
 * The transport wins on RTT, since PROBE's round trip includes the peer's application
 * layer. The receive rate comes only from PROBE: the transport has no idea how fast the
 * peer thinks it is sending.
 *
 * @internal
 */
export function mergeStats(transport: Stats, probe: ProbeStats): Stats {
	return {
		...transport,
		rtt: transport.rtt ?? probe.rtt,
		estimatedRecvRate: probe.estimatedRecvRate,
	};
}

/**
 * Poll the transport's counters into a signal until `closed` settles.
 *
 * The signal starts empty. A transport without `getStats()` (the qmux/WebSocket
 * fallback, older browsers) starts no timer and stays empty forever.
 *
 * @internal
 */
export function pollTransportStats(quic: WebTransport, closed: Promise<unknown>): Getter<Stats> {
	const stats = new Signal<Stats>({});
	if (typeof (quic as { getStats?: unknown }).getStats !== "function") return stats;

	// One request at a time: a getStats() slower than the period would otherwise pile up
	// requests, and an out-of-order completion would overwrite a newer snapshot.
	let pending = false;
	let done = false;

	const poll = async () => {
		if (pending || done) return;
		pending = true;
		try {
			const next = await transportStats(quic);
			// Drop a response that arrives after the connection closed.
			if (!done) stats.set(next ?? {});
		} finally {
			pending = false;
		}
	};

	void poll();
	const id = setInterval(poll, POLL_INTERVAL);
	const stop = () => {
		done = true;
		clearInterval(id);
	};
	closed.then(stop, stop);

	return stats;
}
