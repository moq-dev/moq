import type { Signal } from "@moq/signals";
import type { Announced } from "../announced.ts";
import type { Bandwidth } from "../bandwidth.ts";
import type { Broadcast } from "../broadcast.ts";
import type * as Path from "../path.ts";
import type * as Time from "../time.ts";

/** An established MoQ session, implemented by both the moq-lite and moq-ietf protocols. */
export interface Established {
	/** URL of the connected server. */
	readonly url: URL;

	/** Negotiated wire protocol version. */
	readonly version: string;

	/** Estimated send bitrate from the congestion controller (if supported). */
	readonly sendBandwidth?: Bandwidth;

	/** Estimated receive bitrate from PROBE (moq-lite-03+ only). */
	readonly recvBandwidth?: Bandwidth;

	/** RTT in milliseconds from PROBE (moq-lite-04+ only). */
	readonly rtt?: Signal<Time.Milli | undefined>;

	/** Subscribe to broadcast announcements under an optional path prefix. */
	announced(prefix?: Path.Valid): Announced;

	/** Publish a broadcast at the given path. */
	publish(path: Path.Valid, broadcast: Broadcast): void;

	/** Consume the broadcast at the given path. */
	consume(broadcast: Path.Valid): Broadcast;

	/** Close the session. */
	close(): void;

	/** Resolves when the session closes. */
	closed: Promise<void>;
}
