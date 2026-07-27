import type { Time } from "@moq/net";
import * as Moq from "@moq/net";
import { Effect, type Getter, type GetterInit, getter, Signal } from "@moq/signals";

import type { Format } from "./format";
import type { BufferedRanges, Frame } from "./types";

/** Options for constructing a {@link Consumer}. */
export interface ConsumerProps {
	/** The container format used to decode each MoQ frame. */
	format: Format;
	/** Target latency in milliseconds, controlling how aggressively slow groups are skipped (default: 0). */
	// Read-only: a Getter (e.g. another component's output) is accepted directly.
	latency?: GetterInit<Time.Milli>;
}

interface Group {
	consumer: Moq.Group.Consumer;
	frames: Frame[]; // decode order
	latest?: Time.Micro; // The timestamp of the latest known frame
	end?: Time.Micro; // The furthest presentation point so far, i.e. max(timestamp + duration)
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

// Two adjacent groups are treated as timeline-contiguous when the next group's first PTS is within
// this slack of the current group's end. Per-sample durations and base-decode-times are each rounded
// to microseconds independently, so a genuinely contiguous boundary can be off by ~1µs (seen on 48kHz
// audio). A real missing group spans ~one group duration (orders of magnitude larger), so 1ms cleanly
// separates rounding noise from an actual gap.
const CONTIGUITY_TOLERANCE = Moq.Time.Micro.fromMilli(1 as Time.Milli);

/**
 * True when `nextStart` continues the timeline that ends at `end`: it lands at or before `end`,
 * within CONTIGUITY_TOLERANCE to absorb the µs rounding of independently-rounded per-sample
 * durations and base-decode-times. Undefined on either side means continuity can't be proven.
 *
 * The bound is one-sided (upper only) by design: a next start at or before `end` continues the
 * timeline, a start past `end` beyond the tolerance is a gap. A start well before `end` (a backward
 * jump) is a rewind, handled by Rewind / #checkReset, not here.
 */
function ptsContiguous(end: Time.Micro | undefined, nextStart: Time.Micro | undefined): boolean {
	return end !== undefined && nextStart !== undefined && nextStart <= Moq.Time.Micro.add(end, CONTIGUITY_TOLERANCE);
}

/**
 * True when `next` continues `prev`'s presentation timeline, i.e. nothing is missing between them.
 * Either its sequence is the very next one, which is the only proof available for containers that
 * carry no per-frame duration (Legacy), or the PTS timeline is unbroken across the boundary, which
 * is what non-sequential group numbering (e.g. DTS-derived ids) needs.
 */
function continues(prev: Group, next: Group | undefined): next is Group {
	if (next === undefined) return false;
	return (
		next.consumer.sequence === prev.consumer.sequence + 1 || ptsContiguous(prev.end, next.frames.at(0)?.timestamp)
	);
}

/** Reads frames from a MoQ track in order, buffering groups and skipping slow ones to meet the latency target. */
export class Consumer {
	#track: Moq.Track.Subscriber;
	#format: Format;
	#latency: Getter<Time.Milli>;
	#groups: Group[] = [];
	#active?: number; // the active group sequence number
	// Presentation end (max PTS + duration) of the group we most recently advanced past, so next()'s
	// promotion guard can tell a timeline-continuous next group from one sitting after a gap.
	// Maintained only via #recordPresented; see its comment for the invariant.
	#presentedEnd?: Time.Micro;
	// Group of the last frame next() returned, so it can report whether the following result
	// continues that frame's timeline. Undefined until the first delivery and after a rewind.
	#deliveredGroup?: number;
	// Set whenever the consumer throws content away: a slow group skipped to meet the latency
	// target, a group truncated by a decode error, a reneged straggler, a rewind. Reported (and
	// cleared) on the first frame delivered from the next group, which is where the missing span
	// sits. Only the consumer can know this, which is why next() reports it instead of leaving
	// callers to guess from group numbers.
	#gap = false;
	#rewind = new Rewind(); // live edge + active boundary + discontinuity count

	// Wake up the consumer when a new frame is available.
	#notify?: () => void;

	#buffered = new Signal<BufferedRanges>([]);
	/** The time ranges currently buffered and ready to play. */
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#signals = new Effect();

	/** Start consuming the given track, decoding frames with `props.format`. */
	constructor(track: Moq.Track.Subscriber, props: ConsumerProps) {
		this.#track = track;
		this.#format = props.format;
		this.#latency = getter(props.latency ?? Moq.Time.Milli.zero);

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
				console.warn(`skipping old group: track=${this.#track.name} ${consumer.sequence}`);
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

				const decoded = this.#format.decode(next.payload);

				for (const sample of decoded) {
					const frame: Frame = {
						payload: sample.payload,
						timestamp: sample.timestamp,
						// Protocol invariant: groups always start at a keyframe.
						// For index 0, we enforce this regardless of what the format reports.
						// For index > 0, we trust the format's keyframe detection.
						keyframe: index === 0 ? true : sample.keyframe,
						// Carry the container's per-sample duration through so group.end is the real
						// presentation end (ts + duration), not just the last frame's ts. This is what
						// makes the PTS-contiguity check (next.firstPTS <= group.end) work; without it a
						// contiguous next group looks one frame past the end. Undefined for Legacy (no duration).
						duration: sample.duration,
					};

					index++;

					group.frames.push(frame);

					if (group.latest === undefined || frame.timestamp > group.latest) {
						group.latest = frame.timestamp;
					}

					const end = (frame.timestamp + (frame.duration ?? 0)) as Time.Micro;
					if (group.end === undefined || end > group.end) {
						group.end = end;
					}

					this.#updateBuffered();

					let skipped = false;
					if (group.consumer.sequence !== this.#active) {
						// A non-active group: resolve it against an active reset (dropping a
						// reneged straggler), else detect a new rewind, then check latency. This
						// runs even when the group is the delivery head, because that is exactly
						// the stalled case (#active sits below every buffered group) where the
						// latency budget is what eventually breaks the stall.
						if (this.#classifyStale(group)) return;
						this.#checkReset(group);
						this.#checkLatency();

						// A newer group reaching back to where the stalled active group has
						// already presented means we can advance now instead of waiting.
						skipped = this.#tryDurationSkip();
					}

					// Wake next() for the current delivery head so its frames surface as they
					// arrive. Gating only on `=== #active` assumed +1 group numbering: with
					// non-sequential group ids (large jumps between groups) #active lags one group
					// behind as a stale `+1` phantom, so the real head never matched and its whole
					// group was held until completion, then flushed in a burst. The earliest
					// buffered group is the delivery head regardless of id scheme; next()'s
					// promotion guard advances #active to it. Works for sequential and
					// non-sequential ids alike.
					if (skipped || group.consumer.sequence === this.#active || group === this.#groups[0]) {
						this.#notify?.();
						this.#notify = undefined;
					}
				}
			}
		} catch (_err) {
			// Stop reading the group but keep already-decoded frames.
			// A decode error or stream RESET truncates the tail of the GoP;
			// frames decoded before the error are still valid and playable.
			// The tail is gone though, so the next group does not continue this one.
			this.#gap = true;
		} finally {
			group.done = true;

			if (group.consumer.sequence === this.#active) {
				this.#recordPresented(group);

				// Advance to the next buffered group's actual sequence, but ONLY if it continues this
				// group's timeline. Some encoders number groups non-sequentially with large gaps (not
				// +1), so a bare `+= 1` would point #active at a nonexistent sequence and stall next()
				// until #checkLatency skipped it -- every group through the skip path, i.e. constant
				// stutter. A real PTS gap is different: an intermediate group may still be in transit,
				// so fall back to +1 there (next()'s promotion guard fixes it up once a continuous
				// group arrives) and let #checkLatency / #tryDurationSkip skip the gap only once
				// latency or duration coverage proves it too old.
				const next = this.#groups[this.#groups.indexOf(group) + 1];
				this.#active = continues(group, next) ? next.consumer.sequence : group.consumer.sequence + 1;
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

	// Record where a group's content ends as the cursor advances past it. next()'s promotion guard
	// compares the following group's first PTS against this to tell an unbroken timeline from a real
	// gap, so EVERY site that moves #active past a group must call this; a site that forgets leaves a
	// stale end behind and silently blocks the next contiguous group forever. A group with no frames
	// (empty, or errored before the first one) says nothing about the timeline, so it leaves the last
	// known end in place rather than wiping it.
	#recordPresented(group: Group): void {
		if (group.end !== undefined) this.#presentedEnd = group.end;
	}

	// Whether delivering from group `sequence` continues the timeline of the last frame returned.
	// Frames within a group are consecutive by protocol, so only a group boundary can break it, and
	// there it comes down to whether anything was dropped in between. Deliberately not derived from
	// group numbers: they need not be sequential, so adjacency neither proves continuity nor catches
	// a group the latency check truncated on the way past.
	#continuesDelivery(sequence: number): boolean {
		if (this.#deliveredGroup === undefined) return false;
		return sequence === this.#deliveredGroup || !this.#gap;
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
			console.warn(
				`skipping slow group: track=${this.#track.name} ${first.consumer.sequence} -> ${this.#active}`,
			);

			first.consumer.close();
			first.frames.length = 0;
			skipped = true;
			this.#gap = true;
		}

		if (skipped) {
			this.#updateBuffered();

			// Wake up any consumers waiting for a new frame.
			this.#notify?.();
			this.#notify = undefined;
		}
	}

	// Skip the stalled active group once it has presented up to where the next group
	// begins (its furthest frame end reaches the next group's first timestamp). Only
	// fires when the active group is fully consumed and still open, so we never drop
	// frames the consumer hasn't seen. Returns true if a group was skipped.
	#tryDurationSkip(): boolean {
		if (this.#active === undefined) return false;

		const active = this.#groups[0];
		if (!active || active.consumer.sequence !== this.#active) return false;
		if (active.done || active.frames.length > 0 || active.end === undefined) return false;

		const next = this.#groups[1];
		const nextStart = next?.frames.at(0)?.timestamp;
		if (!next || nextStart === undefined || active.end < nextStart) return false;

		this.#groups.shift();
		console.warn(`skipping covered group: ${active.consumer.sequence} -> ${next.consumer.sequence}`);
		this.#recordPresented(active);
		this.#active = next.consumer.sequence;

		active.consumer.close();
		active.frames.length = 0;
		this.#updateBuffered();
		return true;
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
		this.#gap = true;

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

		console.warn(
			`buffer reset: track=${this.#track.name} group timestamps rewound (prevMax ${reset.prevMax}, group ${reset.group})`,
		);

		// Resume from the earliest survivor; if none, from the rewound group. Anchor the live
		// edge at the rewind point (delivered), not group.latest (buffered ahead of playback).
		this.#active = this.#groups[0]?.consumer.sequence ?? reset.group;
		// Clear the presented-timeline end. After a rewind the old value is a stale, higher timeline
		// end; since the promotion guard's contiguity check is upper-bound only (see ptsContiguous), a
		// new higher-sequence group with a low post-rewind PTS would otherwise look contiguous and be
		// promoted across the discontinuity. Undefined means "unknown", so the guard waits until the
		// resumed group's end is recomputed as it delivers.
		this.#presentedEnd = undefined;
		// Nothing on the new timeline has been delivered yet, so the next frame continues nothing.
		this.#deliveredGroup = undefined;
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
		this.#gap = true;
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
	 *
	 * `continuous` is true when this result picks up exactly where the previous frame left off, so
	 * the span between them can be treated as delivered. It is false on the first frame, after a
	 * rewind, and whenever the consumer threw content away to keep up: a slow group skipped for the
	 * latency target, a group truncated by a decode error, a reneged straggler. Use it rather than
	 * comparing group numbers, which are not required to be sequential: adjacency neither proves
	 * the timeline is unbroken nor catches a group dropped on the way past.
	 *
	 * It reports what this consumer dropped, not holes the publisher left. A publisher that jumps
	 * its timeline between two groups still reads as continuous, because nothing on the wire says
	 * the missing span will never arrive.
	 */
	async next(): Promise<
		{ frame: Frame | undefined; group: number; discontinuity: number; continuous: boolean } | undefined
	> {
		for (;;) {
			// A group may have buffered a rewind while the live edge was still behind it; catch it
			// now that delivery has advanced the edge.
			this.#checkBufferedReset();

			// If #active points below all buffered groups -- e.g. the finally block's `+ 1`
			// fallback fired because no later group was buffered yet, and the real (large-gap,
			// non-sequential) next group has since arrived -- promote #active to the first real
			// group so delivery resumes instead of stalling on a nonexistent sequence.
			// Promote #active up to the first buffered group ONLY when it continues the timeline we left
			// off at (PTS-contiguous with #presentedEnd). Otherwise a real gap sits before it and an
			// in-transit group may still arrive, so wait -- #checkLatency skips the gap once the buffered
			// span exceeds the latency budget, and #tryDurationSkip once the duration covers it.
			if (this.#active !== undefined && this.#groups.length > 0) {
				const head = this.#groups[0];
				if (
					head.consumer.sequence > this.#active &&
					ptsContiguous(this.#presentedEnd, head.frames.at(0)?.timestamp)
				) {
					this.#active = head.consumer.sequence;
				}
			}

			if (
				this.#groups.length > 0 &&
				this.#active !== undefined &&
				this.#groups[0].consumer.sequence <= this.#active
			) {
				const frame = this.#groups[0].frames.shift();
				if (frame) {
					const seq = this.#groups[0].consumer.sequence;
					const continuous = this.#continuesDelivery(seq);
					if (seq !== this.#deliveredGroup) this.#gap = false;
					this.#deliveredGroup = seq;

					// Track the live edge (max timestamp + its group) so a later backwards jump
					// is detectable and the old epoch's tail is anchored.
					const live = this.#rewind.liveEdge;
					if (live === undefined || frame.timestamp > live.timestamp) {
						this.#rewind.liveEdge = { group: seq, timestamp: frame.timestamp };
					}
					this.#updateBuffered();
					return { frame, group: seq, discontinuity: this.#rewind.discontinuity, continuous };
				}

				// Check if the group is done and then remove it.
				// A group is removable when #active has advanced past it, OR when
				// its #runGroup task has finished (done) and all frames are consumed.
				// The latter handles the case where #runGroup finished before
				// #active reached this group (e.g. after a latency skip).
				if (this.#active > this.#groups[0].consumer.sequence || this.#groups[0].done) {
					if (this.#groups[0].consumer.sequence === this.#active) {
						// The cursor moves past this group here rather than in #runGroup's finally
						// block whenever the group finished before it became active, so this is the
						// site that has to record its presentation end. Advance by +1 and let the
						// promotion guard above resolve the real successor on the next iteration.
						this.#recordPresented(this.#groups[0]);
						this.#active += 1;
					}

					const group = this.#groups.shift();
					if (group) {
						const seq = group.consumer.sequence;
						this.#updateBuffered();
						return {
							frame: undefined,
							group: seq,
							discontinuity: this.#rewind.discontinuity,
							// A marker carries no content of its own, so this just reports whether
							// the group it closes was itself reached without a gap.
							continuous: this.#continuesDelivery(seq),
						};
					}
				}

				// The active group is stalled with nothing buffered. If a later group
				// has already been reached by this group's duration, skip ahead now
				// rather than waiting for the stalled group to resume.
				if (this.#tryDurationSkip()) continue;
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
