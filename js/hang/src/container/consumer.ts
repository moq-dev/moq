import type { Time } from "@moq/net";
import * as Moq from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";

import type { Format } from "./format";
import type { BufferedRanges, Frame } from "./types";

/** Options for constructing a {@link Consumer}. */
export interface ConsumerProps {
	/** The container format used to decode each MoQ frame. */
	format: Format;
	/** Target latency in milliseconds, controlling how aggressively slow groups are skipped (default: 0). */
	latency?: Signal<Time.Milli> | Time.Milli;
}

interface Group {
	consumer: Moq.Group;
	frames: Frame[]; // decode order
	latest?: Time.Micro; // The timestamp of the latest known frame
	done?: boolean; // Set when #runGroup finishes reading all frames
}

/**
 * A recorded rewind boundary.
 *
 * After a backwards timestamp jump, groups can still arrive out of order, so a single
 * sequence floor is not enough: a late new-epoch group can have a lower sequence than the
 * group that triggered detection. This keeps just enough state to classify any group by
 * its (sequence, timestamp).
 */
class Reset {
	/** Highest-sequence old-epoch group seen at detection. At or below this is old: drop. */
	readonly prevMax: number;
	/** The group whose backwards timestamp triggered detection. At or above this is new: keep. */
	readonly group: number;
	/** That group's timestamp; in the ambiguous span, old stragglers sit at or above it. */
	readonly timestamp: Time.Micro;

	constructor(prevMax: number, group: number, timestamp: Time.Micro) {
		this.prevMax = prevMax;
		this.group = group;
		this.timestamp = timestamp;
	}

	/** Classify by sequence alone: true=old, false=new, undefined=ambiguous (resolve by timestamp). */
	bySequence(sequence: number): boolean | undefined {
		if (sequence <= this.prevMax) return true;
		if (sequence >= this.group) return false;
		return undefined;
	}

	/** Whether a group belongs to the reneged old epoch and should be dropped. */
	isStale(sequence: number, timestamp: Time.Micro): boolean {
		return this.bySequence(sequence) ?? timestamp >= this.timestamp;
	}
}

/**
 * Live state for detecting timeline rewinds and classifying out-of-order groups.
 *
 * A publisher reneges its buffered tail by rewinding timestamps while group sequence keeps
 * climbing (e.g. a voice agent interrupted mid-utterance). We track the live edge to spot the
 * jump, a {@link Reset} boundary to classify out-of-order groups across it, and a counter that
 * downstream consumers watch to flush their own queues.
 */
class Rewind {
	/** The live edge of playback: max delivered timestamp and the group that carried it. */
	liveEdge?: { group: number; timestamp: Time.Micro };
	/** The active rewind boundary, if any. */
	boundary?: Reset;
	/** Increments on every rewind; downstream consumers flush their queues when it changes. */
	discontinuity = 0;
}

/** Reads frames from a MoQ track in order, buffering groups and skipping slow ones to meet the latency target. */
export class Consumer {
	#track: Moq.Track;
	#format: Format;
	#latency: Signal<Time.Milli>;
	#groups: Group[] = [];
	#active?: number; // the active group sequence number
	#rewind = new Rewind(); // live edge + active boundary + discontinuity count

	// Wake up the consumer when a new frame is available.
	#notify?: () => void;

	#buffered = new Signal<BufferedRanges>([]);
	/** The time ranges currently buffered and ready to play. */
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#signals = new Effect();

	/** Start consuming the given track, decoding frames with `props.format`. */
	constructor(track: Moq.Track, props: ConsumerProps) {
		this.#track = track;
		this.#format = props.format;
		this.#latency = Signal.from(props.latency ?? Moq.Time.Milli.zero);

		this.#signals.spawn(this.#run.bind(this));
		this.#signals.cleanup(() => {
			this.#track.close();
			for (const group of this.#groups) {
				group.consumer.close();
			}
			this.#groups.length = 0;
		});
	}

	async #run() {
		// Start fetching groups in the background
		for (;;) {
			const consumer = await this.#track.recvGroup();
			if (!consumer) break;

			// To improve TTV, we always start with the first group.
			// For higher latencies we might need to figure something else out, as its racey.
			if (this.#active === undefined) {
				this.#active = consumer.sequence;
			}

			// Normally we drop anything behind the cursor. With an active reset the cursor isn't
			// a valid floor (a late new-epoch group can sit below it); defer to the boundary and
			// admit ambiguous groups so #runGroup can rule on them once their timestamps arrive.
			let drop: boolean;
			if (this.#rewind.boundary) {
				const verdict = this.#rewind.boundary.bySequence(consumer.sequence);
				if (verdict === undefined) drop = false;
				else if (verdict) drop = true;
				else drop = consumer.sequence < this.#active;
			} else {
				drop = consumer.sequence < this.#active;
			}

			if (drop) {
				console.warn(`skipping old group: ${consumer.sequence}`);
				consumer.close();
				continue;
			}

			const group: Group = {
				consumer,
				frames: [],
			};

			// Insert into #groups based on the group sequence number (ascending).
			// This is used to cancel old groups.
			this.#groups.push(group);
			this.#groups.sort((a, b) => a.consumer.sequence - b.consumer.sequence);

			// Start buffering frames from this group
			this.#signals.spawn(this.#runGroup.bind(this, group));
		}
	}

	async #runGroup(group: Group) {
		try {
			let index = 0;

			for (;;) {
				const next = await group.consumer.readFrame();
				if (!next) break;

				const decoded = this.#format.decode(next);

				for (const sample of decoded) {
					const frame: Frame = {
						data: sample.data,
						timestamp: sample.timestamp,
						// Protocol invariant: groups always start at a keyframe.
						// For index 0, we enforce this regardless of what the format reports.
						// For index > 0, we trust the format's keyframe detection.
						keyframe: index === 0 ? true : sample.keyframe,
					};

					index++;

					group.frames.push(frame);

					if (group.latest === undefined || frame.timestamp > group.latest) {
						group.latest = frame.timestamp;
					}

					this.#updateBuffered();

					if (group.consumer.sequence === this.#active) {
						this.#notify?.();
						this.#notify = undefined;
					} else {
						// Newer group: resolve it against an active reset (dropping a reneged
						// straggler), else detect a new rewind, then check latency.
						if (this.#classifyStale(group)) return;
						this.#checkReset(group);
						this.#checkLatency();
					}
				}
			}
		} catch (_err) {
			// Stop reading the group but keep already-decoded frames.
			// A decode error or stream RESET truncates the tail of the GoP;
			// frames decoded before the error are still valid and playable.
		} finally {
			group.done = true;

			if (group.consumer.sequence === this.#active) {
				// Advance to the next group.
				this.#active += 1;
			}

			// Recompute buffered ranges now that this group is done,
			// so consecutive done groups can merge into a single range.
			this.#updateBuffered();

			// Always notify - the consumer may need to advance past this group
			// even if it wasn't active when this task finished.
			this.#notify?.();
			this.#notify = undefined;

			group.consumer.close();
		}
	}

	#checkLatency() {
		if (this.#active === undefined) return;

		let skipped = false;

		// Keep skipping the oldest group while the buffered span exceeds the latency target.
		// This also handles gaps in group sequence numbers: if #active points to a missing
		// group, the latency span proves the missing content is too old to wait for.
		while (this.#groups.length >= 2) {
			const threshold = Moq.Time.Micro.fromMilli(this.#latency.peek());

			// Check the difference between the earliest and latest known frames.
			let min: number | undefined;
			let max: number | undefined;

			for (const group of this.#groups) {
				if (group.latest === undefined) continue;

				const frame = group.frames.at(0)?.timestamp ?? group.latest;
				if (min === undefined || frame < min) min = frame;
				if (max === undefined || group.latest > max) max = group.latest;
			}

			if (min === undefined || max === undefined) break;

			const latency = max - min;
			if (latency <= threshold) break;

			const first = this.#groups.shift();
			if (!first) break;
			this.#active = this.#groups[0]?.consumer.sequence;
			console.warn(`skipping slow group: ${first.consumer.sequence} -> ${this.#active}`);

			first.consumer.close();
			first.frames.length = 0;
			skipped = true;
		}

		if (skipped) {
			this.#updateBuffered();

			// Wake up any consumers waiting for a new frame.
			this.#notify?.();
			this.#notify = undefined;
		}
	}

	// Detect a publisher "rewind" and record the reneged boundary. A newer group (sequence
	// climbs) whose earliest frame lands before the live edge (timestamp goes backwards) can
	// only be an explicit reneg of the buffered tail; record the boundary, bump the
	// discontinuity counter, drop the groups it proves stale, and resume from the earliest
	// survivor. Groups still ambiguous (a late new-epoch group vs. an old straggler) are kept
	// and resolved by #classifyStale once their timestamps arrive.
	#checkReset(group: Group) {
		if (this.#active === undefined) return;

		const live = this.#rewind.liveEdge;
		if (live === undefined) return;

		// Only a group newer than the active one can rewind the timeline.
		if (group.consumer.sequence <= this.#active) return;

		const start = group.frames.at(0)?.timestamp;
		if (start === undefined) return;

		// A rewind is the timestamp going strictly backwards past the live edge. Anything at
		// or ahead of it is normal forward motion.
		if (start >= live.timestamp) return;

		const reset = new Reset(live.group, group.consumer.sequence, start);
		this.#rewind.boundary = reset;
		this.#rewind.discontinuity++;

		// Drop buffered groups the boundary can already prove stale; keep ambiguous ones.
		this.#groups = this.#groups.filter((g) => {
			const verdict = reset.bySequence(g.consumer.sequence);
			const first = g.frames.at(0);
			const stale = verdict ?? (first !== undefined && reset.isStale(g.consumer.sequence, first.timestamp));
			if (stale) {
				g.consumer.close();
				g.frames.length = 0;
			}
			return !stale;
		});

		console.warn(`buffer reset: group timestamps rewound (prevMax ${reset.prevMax}, group ${reset.group})`);

		// Resume from the earliest survivor; if none, from the rewound group. Anchor the live
		// edge at the rewind point (delivered), not group.latest (buffered ahead of playback).
		this.#active = this.#groups[0]?.consumer.sequence ?? reset.group;
		this.#rewind.liveEdge = { group: reset.group, timestamp: start };
		this.#updateBuffered();

		// Wake up any consumer waiting for a new frame.
		this.#notify?.();
		this.#notify = undefined;
	}

	// Drop a group that an active reset resolves as a reneged old straggler (its timestamp
	// landed at or above the reset point). Returns true if the group was dropped.
	#classifyStale(group: Group): boolean {
		const reset = this.#rewind.boundary;
		if (!reset) return false;

		const first = group.frames.at(0);
		if (first === undefined) return false;
		if (!reset.isStale(group.consumer.sequence, first.timestamp)) return false;

		this.#groups = this.#groups.filter((g) => g !== group);
		group.consumer.close();
		group.frames.length = 0;
		this.#updateBuffered();
		return true;
	}

	// Re-check buffered newer groups against the current live edge. #checkReset otherwise only
	// runs when a group receives a frame, so a group that buffered while the live edge was lower
	// (or undefined) is never reconsidered once delivery advances the edge past it, and the
	// rewind would be missed. Highest sequence first, mirroring the Rust scan: the first rewound
	// group becomes the boundary, and #checkReset's own guards make the rest no-ops.
	#checkBufferedReset(): void {
		if (this.#active === undefined || this.#rewind.liveEdge === undefined) return;
		for (const group of [...this.#groups].reverse()) {
			if (group.consumer.sequence <= this.#active) break;
			this.#checkReset(group);
		}
	}

	/**
	 * Returns the next frame in order along with its group number and the current
	 * {@link discontinuity} count, awaiting one if needed. A `frame` of undefined signals the
	 * end of that group; the overall result is undefined once closed. When `discontinuity`
	 * jumps relative to the previous call, the publisher rewound the timeline: flush any
	 * downstream decoder or render buffers before playing this frame.
	 */
	async next(): Promise<{ frame: Frame | undefined; group: number; discontinuity: number } | undefined> {
		for (;;) {
			// A group may have buffered a rewind while the live edge was still behind it; catch it
			// now that delivery has advanced the edge.
			this.#checkBufferedReset();

			if (
				this.#groups.length > 0 &&
				this.#active !== undefined &&
				this.#groups[0].consumer.sequence <= this.#active
			) {
				const frame = this.#groups[0].frames.shift();
				if (frame) {
					const seq = this.#groups[0].consumer.sequence;
					// Track the live edge (max timestamp + its group) so a later backwards jump
					// is detectable and the old epoch's tail is anchored.
					const live = this.#rewind.liveEdge;
					if (live === undefined || frame.timestamp > live.timestamp) {
						this.#rewind.liveEdge = { group: seq, timestamp: frame.timestamp };
					}
					this.#updateBuffered();
					return { frame, group: seq, discontinuity: this.#rewind.discontinuity };
				}

				// Check if the group is done and then remove it.
				// A group is removable when #active has advanced past it, OR when
				// its #runGroup task has finished (done) and all frames are consumed.
				// The latter handles the case where #runGroup finished before
				// #active reached this group (e.g. after a latency skip).
				if (this.#active > this.#groups[0].consumer.sequence || this.#groups[0].done) {
					if (this.#groups[0].consumer.sequence === this.#active) {
						this.#active += 1;
					}

					const group = this.#groups.shift();
					if (group) {
						this.#updateBuffered();
						return {
							frame: undefined,
							group: group.consumer.sequence,
							discontinuity: this.#rewind.discontinuity,
						};
					}
				}
			}

			if (this.#notify) {
				throw new Error("multiple calls to next not supported");
			}

			const abort = this.#signals.abort;
			if (abort.aborted) return undefined;

			const aborted = await new Promise<boolean>((resolve) => {
				const onAbort = () => resolve(true);
				abort.addEventListener("abort", onAbort, { once: true });
				this.#notify = () => {
					abort.removeEventListener("abort", onAbort);
					resolve(false);
				};
			});

			this.#notify = undefined;
			if (aborted) return undefined;
		}
	}

	#updateBuffered(): void {
		const ranges: BufferedRanges = [];

		let prev: Group | undefined;

		for (const group of this.#groups) {
			const first = group.frames.at(0);
			if (!first || group.latest === undefined) continue;

			const start = Moq.Time.Milli.fromMicro(first.timestamp);
			const end = Moq.Time.Milli.fromMicro(group.latest);

			const last = ranges.at(-1);
			const contiguous = prev?.done && prev.consumer.sequence + 1 === group.consumer.sequence;
			if (last && (last.end >= start || contiguous)) {
				last.end = Moq.Time.Milli.max(last.end, end);
			} else {
				ranges.push({ start, end });
			}

			prev = group;
		}

		this.#buffered.set(ranges);
	}

	/**
	 * A counter that increments each time the consumer detects a timeline rewind and drops the
	 * reneged buffer. Also surfaced per-read via {@link next}; downstream consumers flush their
	 * decoder and render buffers when it changes.
	 */
	get discontinuity(): number {
		return this.#rewind.discontinuity;
	}

	/** Stop consuming and release the track and all buffered groups. */
	close(): void {
		this.#signals.close();
	}
}
