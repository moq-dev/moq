/**
 * Probing a browser session up to its target with padding.
 *
 * A browser can't reach its WebTransport congestion controller, and the controller stops
 * learning once the session is app-limited, so a session that wants more than it sends
 * would never find out the path has room. This sender fills that gap the way GoogCC's
 * `ProbeController` does: while app-limited below the target it sends a short cluster of
 * padding about every five seconds, at twice the estimate capped at the target, and a
 * follow-up at once when a cluster validates. Padding is ordinary stream data at the
 * lowest send order, so the controller counts and paces it, and media preempts it locally.
 *
 * @module
 */
import { Instant, Milli, Nano, now } from "@moq/clock";
import { Effect, Signal } from "@moq/signals";
import { transportStats } from "./connection/stats.ts";
import { type StreamVersion, Writer } from "./stream.ts";

/** How often the sender samples the transport and decides what to pad. */
const TICK = Nano.fromMilli(Milli(100));

/** How long the sender waits between clusters while app-limited, GoogCC's ALR probing interval. */
const PERIOD = Nano.fromMilli(Milli(5000));

/**
 * The longest a cluster pads.
 *
 * Longer than GoogCC's, whose clusters it judges per packet: this one is judged by the
 * transport's estimate, sampled every {@link TICK}, so it has to last a few samples.
 */
const CLUSTER = Nano.fromMilli(Milli(500));

/** How long after a cluster ends its estimate may still validate it, as GoogCC waits for a probe result. */
const VALIDATE = Nano.fromMilli(Milli(1000));

/** A cluster's rate as a multiple of the estimate, GoogCC's ALR probe scale. */
const SCALE = 2;

/** The share of a cluster's rate the estimate must reach to validate it, GoogCC's further-probe threshold. */
const FURTHER = 0.7;

/**
 * The app-limited detector, GoogCC's `AlrDetector`: a budget filling at this share of the
 * estimate and drained by media, over {@link ALR_WINDOW}.
 */
const ALR_USAGE = 0.65;
const ALR_WINDOW = Nano.fromMilli(Milli(500));
/** The budget level that starts the app-limited state, and the one that ends it. */
const ALR_START = 0.8;
const ALR_STOP = 0.5;

/**
 * Padding's average rate at most, as a share of the estimate: native probing's 10% budget,
 * taken of what the path carries rather than of the media, which an app-limited session
 * sends far less of than a cluster needs.
 */
const OVERHEAD = 0.1;

/** The transport send order for padding: below every group stream, so media always goes first. */
export const ORDER = Number.MIN_SAFE_INTEGER;

/** The largest padding write, so a write never holds a long run of padding the transport must accept at once. */
const CHUNK = new Uint8Array(16 * 1024);

/**
 * What a sender is handed: the rate it probes up to, which the session sets from its
 * allocator's target. Mirrors Rust's `transport::Probe`.
 *
 * @internal
 */
export interface Sender {
	/** Probe up to `rate` bits per second; zero stops probing. */
	setTarget(rate: number): void;
}

/** What the schedule reads from the transport each tick. */
export interface Sample {
	/** The rate to probe up to, in bits per second. */
	target: number;
	/** The congestion controller's estimate in bits per second, if the transport reports one. */
	estimate?: number;
	/** Total bytes the transport has sent, padding included, if it reports them. */
	sent?: number;
}

interface Cluster {
	start: Instant;
	/** The rate the cluster pads up to, media included. */
	rate: number;
	/** The estimate it started from, which a falling estimate drops below. */
	estimate: number;
}

/**
 * When to pad and how much, decided once per tick from what the transport reports.
 *
 * App-limited is judged from media alone, everything sent except the padding handed to
 * the transport, so a cluster never ends the state that started it. Padding draws on a
 * credit earning {@link OVERHEAD} of the estimate, capped at one cluster at the target. A
 * cluster starts only once the credit funds its first tick, and ends once it runs out, so
 * a path with no room is never padded past that share however long it stays shut.
 *
 * @internal
 */
export class Schedule {
	#last?: { at: Instant; sent: number; padded: number };
	#padded = 0;
	/** Media's rate over the last tick, in bits per second. */
	#media = 0;
	/** The app-limited detector's budget, in bytes. */
	#budget = 0;
	/** When the session became app-limited, if it is. */
	#limited?: Instant;
	/** The padding bytes the overhead budget still allows. */
	#credit = 0;
	#cluster?: Cluster;
	/** A cluster that ended, waiting for the estimate to validate it. */
	#validating?: { until: Instant; rate: number };
	/** A validated cluster's follow-up, waiting for credit. */
	#followUp = false;
	/** When the last cluster started. */
	#probed?: Instant;

	/** Count `bytes` of padding handed to the transport. */
	padded(bytes: number): void {
		this.#padded += bytes;
		this.#credit -= bytes;
	}

	/** Whether a cluster is padding. */
	get padding(): boolean {
		return this.#cluster !== undefined;
	}

	/** How many padding bytes to send before the next tick, from what the transport reports now. */
	tick({ target, estimate, sent }: Sample): number {
		const at = now();
		if (sent === undefined || estimate === undefined || estimate <= 0) {
			// Nothing to judge by: start over once the transport reports again.
			this.#last = undefined;
			this.#limited = undefined;
			this.#stop();
			return 0;
		}

		if (this.#last) {
			const elapsed = Nano.toSecond(Instant.durationSince(at, this.#last.at));
			const media = Math.max(0, sent - this.#last.sent - (this.#padded - this.#last.padded));
			if (elapsed > 0) this.#media = (media * 8) / elapsed;
			this.#detect(estimate, media, elapsed);
			const cap = (target / 8) * Nano.toSecond(CLUSTER);
			this.#credit = Math.min(this.#credit + (estimate / 8) * OVERHEAD * elapsed, cap);
		}
		this.#last = { at, sent, padded: this.#padded };

		if (target <= estimate || this.#limited === undefined) {
			this.#stop();
			return 0;
		}

		const cluster = this.#cluster;
		if (cluster) {
			if (estimate < cluster.estimate) {
				// The path pushed back: end without a follow-up.
				this.#stop();
				return 0;
			}
			const bytes = this.#pad(cluster.rate);
			if (Instant.durationSince(at, cluster.start) < CLUSTER && bytes > 0) return bytes;
			this.#cluster = undefined;
			this.#validating = { until: Instant.add(at, VALIDATE), rate: cluster.rate };
		}

		const validating = this.#validating;
		if (validating) {
			if (estimate >= validating.rate * FURTHER) {
				this.#validating = undefined;
				this.#followUp = true;
			} else if (at < validating.until) {
				return 0;
			} else {
				this.#validating = undefined;
			}
		}

		if (!this.#followUp) {
			const since = this.#probed === undefined || this.#probed < this.#limited ? this.#limited : this.#probed;
			if (Instant.durationSince(at, since) < PERIOD) return 0;
		}

		// Wait until the credit funds a whole first tick, rather than starting a cluster too
		// thin to move the estimate.
		const rate = Math.min(estimate * SCALE, target);
		const bytes = this.#pad(rate, Number.POSITIVE_INFINITY);
		if (bytes <= 0 || bytes > this.#credit) return 0;

		this.#followUp = false;
		this.#probed = at;
		this.#cluster = { start: at, rate, estimate };
		return bytes;
	}

	#stop() {
		this.#cluster = undefined;
		this.#validating = undefined;
		this.#followUp = false;
	}

	// What fills a tick up to `rate` beside the media, within `credit`.
	#pad(rate: number, credit = this.#credit): number {
		const bytes = ((rate - this.#media) / 8) * Nano.toSecond(TICK);
		return Math.floor(Math.max(0, Math.min(bytes, credit)));
	}

	// Fill the budget at a share of the estimate and drain it by media, entering the
	// app-limited state once it is mostly unspent and leaving once it is mostly spent.
	#detect(estimate: number, media: number, elapsed: number) {
		const fill = (estimate / 8) * ALR_USAGE;
		const max = fill * Nano.toSecond(ALR_WINDOW);
		this.#budget = Math.min(Math.max(this.#budget + fill * elapsed - media, -max), max);
		const level = this.#budget / max;
		if (this.#limited === undefined && level > ALR_START) this.#limited = now();
		else if (this.#limited !== undefined && level < ALR_STOP) this.#limited = undefined;
	}
}

/** What a padding sender writes on. */
export interface PaddingProps {
	/** The session to pad. */
	quic: WebTransport;
	/** The negotiated version, which selects the varint encoding. */
	version: StreamVersion;
	/** The uni stream type that marks a padding stream on this version. */
	type: bigint;
}

/**
 * Pads a WebTransport session up to its target on {@link Schedule}'s schedule.
 *
 * Each cluster opens one padding stream at the lowest send order and ends it with the
 * cluster, resetting it if a write is still queued so no stale padding waits in front of
 * the next frame. It never waits for a stream slot, so padding never takes one media
 * needs.
 *
 * @internal
 */
export class Padding implements Sender {
	#active = new Signal(false);
	#target = 0;
	#signals = new Effect();

	constructor({ quic, version, type }: PaddingProps) {
		const close = () => this.close();
		void quic.closed.then(close, close);

		this.#signals.run((effect) => {
			if (!effect.get(this.#active)) return;

			const schedule = new Schedule();
			let stream: Writer | undefined;
			let opening = false;
			let writing = false;
			let sampling = false;
			let done = false;

			const end = () => {
				if (!stream) return;
				if (writing) stream.reset(new Error("padding cluster ended"));
				else stream.close();
				stream = undefined;
			};
			effect.cleanup(() => {
				done = true;
				end();
			});

			const open = async () => {
				opening = true;
				try {
					const writer = await Writer.tryOpen(quic, {
						version,
						sendOrder: ORDER,
						waitUntilAvailable: false,
						cancel: effect.closed,
					});
					if (!writer) return;
					await writer.u62(type);
					// The cluster, or the whole run, may have ended while the stream opened.
					if (schedule.padding && !done) stream = writer;
					else writer.close();
				} catch {
					// No slot, or the session is going away: this cluster pads nothing.
				} finally {
					opening = false;
				}
			};

			const write = async (writer: Writer, bytes: number) => {
				writing = true;
				try {
					while (bytes > 0) {
						const size = Math.min(bytes, CHUNK.byteLength);
						await writer.write(CHUNK.subarray(0, size));
						bytes -= size;
					}
				} catch {
					// The peer stopped it or the cluster reset it; the next cluster opens another.
					if (stream === writer) stream = undefined;
				} finally {
					writing = false;
				}
			};

			const tick = async () => {
				if (sampling) return;
				sampling = true;
				try {
					const stats = await effect.race(transportStats(quic));
					if (!stats) return;
					const bytes = schedule.tick({
						target: this.#target,
						estimate: stats.estimatedSendRate,
						sent: stats.bytesSent,
					});
					if (!schedule.padding) {
						end();
						return;
					}
					if (!stream) {
						if (!opening) void open();
						return;
					}
					// Still writing the last tick's: the transport is holding it, so add nothing.
					if (bytes === 0 || writing) return;
					schedule.padded(bytes);
					void write(stream, bytes);
				} finally {
					sampling = false;
				}
			};

			void tick();
			effect.interval(() => void tick(), TICK);
		});
	}

	/** Probe up to `rate` bits per second; zero stops probing. */
	setTarget(rate: number): void {
		this.#target = rate;
		this.#active.set(rate > 0);
	}

	/** Stop padding for good. */
	close(): void {
		this.#signals.close();
	}
}
