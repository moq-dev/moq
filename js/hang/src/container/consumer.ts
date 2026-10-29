import { Instant, Micro, Milli, Nano, now } from "@moq/clock";
import type * as Moq from "@moq/net";
import { Effect, type Getter, type GetterInit, getter, Once, Signal } from "@moq/signals";
import type { Format } from "./format";
import { Jitter } from "./jitter";
import type { BufferedRanges, Frame, ReceivedFrame } from "./types";

/** Options for constructing a {@link Consumer}. */
export interface ConsumerProps {
	/** The container format used to decode each MoQ frame. */
	format: Format;
	/**
	 * How stale a group may get before it is skipped, in milliseconds (default: 0).
	 *
	 * Measured as the span from the oldest buffered frame to the newest, so it bounds how long a
	 * late group is waited for. A missing sequence also expires this much monotonic time after
	 * the first group past it arrived, even when the next group is a marker on a paused track.
	 * The local half of the subscription's
	 * `maxDelay`; both measure the same budget, one on the wire and one as frames are read.
	 */
	// Read-only: a Getter (e.g. another component's output) is accepted directly.
	maxDelay?: GetterInit<Milli>;
	/**
	 * Cap the bytes of decoded frames held ahead of the reader, counting the whole wire payload they
	 * share and each frame's and group's own overhead (default: {@link DEFAULT_MAX_BUFFER}). Mirrors
	 * Rust's `Consumer::set_max_buffer`.
	 *
	 * Once it is full the consumer stops decoding ahead and stops pulling new groups past one successor
	 * to the group being read, so the undecoded rest stays in the transport's cache, which bounds it. The group being read still decodes a wire
	 * frame whenever it holds nothing, so reading never waits on its own lookahead. If it stalls while the
	 * cap is full, the consumer moves on to the next group like a skip past a slow group, read or not: no
	 * more of what it waits behind can be held.
	 */
	maxBuffer?: number;
	/**
	 * The consumer this one replaces on the same path, such as the previous rendition.
	 *
	 * Its arrival estimate carries over instead of restarting from the cold-start prior, so a
	 * switch neither lowers nor raises {@link Consumer.spread} on its own.
	 */
	predecessor?: Consumer;
}

/** One item of a {@link Consumer.next} read, in delivery order. Mirrors Rust's `container::Read`. */
export type Read =
	/** A media frame from `group`. Only group equality means anything: numbers may skip. */
	| { kind: "frame"; group: number; frame: ReceivedFrame }
	/**
	 * Media after an item already read will never reach this reader: a group was skipped, evicted,
	 * truncated, or never arrived. A decoder resumes without the references or state that media
	 * built. Joining mid-track is not a gap: nothing was read before it.
	 *
	 * The span is an upper bound. `timestamp` is the furthest point already read; without a
	 * per-frame duration that is the last frame's timestamp, one frame early. `duration` runs to
	 * where the next item resumes, so it also covers any declared pause in between.
	 */
	| { kind: "gap"; timestamp: Micro; duration: Micro }
	/**
	 * The publisher declared that its media stops at `end`: it paused, switched source, or finished.
	 * Frames that follow from the same `group` are the stopped encoder's final packets (an audio
	 * codec's drain), whose output at or past `end` is padding. The next group starts fresh.
	 */
	| { kind: "end"; group: number; end: Micro };

/** The default {@link ConsumerProps.maxBuffer}: one group at the transport's size cap. Mirrors Rust's. */
export const DEFAULT_MAX_BUFFER = 32 * 1024 * 1024;

// Rough sizes of a buffered frame's and a group's own objects, charged to the max buffer alongside
// payload bytes. JS cannot measure them; these only need to be the right order of magnitude.
const FRAME_COST = 64;
const GROUP_COST = 256;

interface Group {
	consumer: Moq.Group.Consumer;
	// From then on, any sequence missing below this group is late: the earliest arrival among it
	// and the groups past it, kept on insert so a hole reads it without scanning the backlog.
	late: Instant;
	frames: ReceivedFrame[]; // decode order
	costs: number[]; // What each of frames holds against the max buffer
	empty: boolean; // no wire frame was published; empty groups mean nothing
	media: boolean; // a decodable (non-marker) frame was buffered
	bare?: boolean; // a data track's marker group: one object with no bytes
	start?: Micro; // First decoded timestamp
	minMedia?: Micro; // Lowest decodable timestamp
	latest?: Micro; // The timestamp of the latest known frame
	end?: Micro; // The furthest presentation point so far, i.e. max(timestamp + duration)
	fault?: Error; // The payload that failed to decode, an error after the buffered frames are delivered.
	index: number; // Media frames decoded so far: the first is a keyframe by protocol invariant
	charged: number; // What this group holds against the max buffer
	released?: boolean; // Dropped from #groups, with what it held refunded
}

// Two adjacent groups are treated as timeline-contiguous when the next group's first PTS is within
// this slack of the current group's end. Per-sample durations and base-decode-times are each rounded
// to microseconds independently, so a genuinely contiguous boundary can be off by ~1µs (seen on 48kHz
// audio). A real missing group spans ~one group duration (orders of magnitude larger), so 1ms cleanly
// separates rounding noise from an actual gap.
const CONTIGUITY_TOLERANCE = Micro.fromMilli(1 as Milli);

/**
 * True when `nextStart` continues the timeline that ends at `end`: it lands at or before `end`,
 * within CONTIGUITY_TOLERANCE to absorb the µs rounding of independently-rounded per-sample
 * durations and base-decode-times. Undefined on either side means continuity can't be proven.
 *
 * The bound is one-sided (upper only) by design: a next start at or before `end` continues the
 * timeline, a start past `end` beyond the tolerance is a gap. Group starts are validated
 * separately; an overlapping endpoint is not a rewind.
 */
function ptsContiguous(end: Micro | undefined, nextStart: Micro | undefined): boolean {
	return end !== undefined && nextStart !== undefined && nextStart <= end + CONTIGUITY_TOLERANCE;
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

/** Reads frames from a MoQ track in order, buffering groups and skipping ones that fall `maxDelay` behind. */
export class Consumer {
	#track: Moq.Track.Subscriber;
	#format: Format;
	#maxDelay: Getter<Milli>;
	#maxBuffer: number;
	// What the buffered groups hold, against the max buffer.
	#used = 0;
	// Group watchers and the track loop parked on a full max buffer.
	#room: (() => void)[] = [];
	#groups: Group[] = [];
	#active?: number; // the active group sequence number
	// When the missing sequence at the cursor became late, while one is.
	#hole?: Instant;
	// Presentation end (max PTS + duration) of the group we most recently advanced past, so next()'s
	// promotion guard can tell a timeline-continuous next group from one sitting after a gap.
	// Maintained only via #recordPresented; see its comment for the invariant.
	#presentedEnd?: Micro;
	// Group of the last media frame next() returned, so an endpoint after it reads as that frame's
	// duration rather than a declared stop.
	#mediaGroup?: number;
	// Media went missing since the last item read, so the next one is preceded by a gap.
	#lost = false;
	// The furthest presentation point read so far: max(timestamp + duration), endpoints included.
	// Undefined until the first item, so a loss before it is no gap.
	#edge?: Micro;
	// The item a gap was returned ahead of.
	#next?: Read;
	// The first media timestamp of the latest delivered group, and its predecessor's start.
	// Endpoints and later frames can overlap the next group without moving this floor.
	#start?: { group: number; timestamp: Micro };
	#floor?: Micro;
	// The error that ended the reader, the first in delivery order: a decode failure or a rewind.
	#error?: Error;

	// Wake up the consumer when a new frame is available.
	#notify?: () => void;

	#buffered = new Signal<BufferedRanges>([]);
	/** The time ranges currently buffered and ready to play. */
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#jitter: Jitter;
	#spread: Signal<Milli>;
	/**
	 * How much buffer late arrivals need, measured as frames come off the transport.
	 *
	 * The measured term of doc/concept/audio-jitter.md, excluding the codec frame and any advertised
	 * floor. Starts at the predecessor's estimate, or the cold-start estimate without one.
	 */
	readonly spread: Getter<Milli>;

	#signals = new Effect();
	#closed = new Once<Error | null>();

	/** Start consuming the given track, decoding frames with `props.format`. */
	constructor(track: Moq.Track.Subscriber, props: ConsumerProps) {
		this.#track = track;
		this.#format = props.format;
		this.#maxDelay = getter(props.maxDelay ?? Milli.zero);
		this.#maxBuffer = props.maxBuffer ?? DEFAULT_MAX_BUFFER;
		this.#jitter = props.predecessor ? props.predecessor.#jitter.fork() : new Jitter();
		this.#spread = new Signal(this.#jitter.measured);
		this.spread = this.#spread;

		this.#signals.spawn(this.#run.bind(this));
		this.#signals.cleanup(() => {
			this.#track.close();
			for (const group of this.#groups) {
				group.consumer.close();
			}
			this.#groups.length = 0;
			// Parked watchers see the abort and return.
			this.#wake();
		});
	}

	#finish(end: Error | null): void {
		if (this.#closed.peek() === undefined) this.#closed.set(end);
		this.#notify?.();
		this.#notify = undefined;
	}

	async #run() {
		// Start fetching groups in the background
		try {
			for (;;) {
				// Past the max buffer, later groups wait in the transport's cache, once the head has a successor
				// to move on to: however small the cap, a stalled head can always be skipped. A missing group
				// at the cursor then expires like one past the budget: see next().
				while (this.#full() && this.#groups.length >= 2 && !this.#signals.abort.aborted) await this.#park();
				if (this.#signals.abort.aborted) break;

				const consumer = await this.#track.recvGroup();
				if (!consumer) break;

				// To improve TTV, we always start with the first group.
				// For higher latencies we might need to figure something else out, as its racey.
				if (this.#active === undefined) {
					this.#active = consumer.sequence;
				}

				// Arriving below the delivery cursor is not a reason to drop a group. Groups are
				// sent newest-first, so the head of a subscription arrives after the live edge it
				// was served alongside. Audio can place older groups in its timestamp-indexed ring;
				// video must reject older groups before decode to preserve codec references. How far back one
				// may be is the subscription's own max delay, applied before it ever reaches here.
				const group: Group = {
					consumer,
					// When the group landed, not when it was pulled: a full max buffer delays pulls.
					late: consumer.arrived,
					frames: [],
					costs: [],
					empty: true,
					media: false,
					index: 0,
					charged: 0,
				};
				this.#charge(group, GROUP_COST);

				// Insert into #groups based on the group sequence number (ascending).
				// This is used to cancel old groups.
				let index = this.#groups.findIndex((other) => other.consumer.sequence > consumer.sequence);
				if (index === -1) index = this.#groups.length;
				// Each group is late from the earliest arrival among it and the groups past it. A drained
				// backlog comes in sequence order, not arrival order, so an arrival can lower those below it.
				const next = this.#groups.at(index);
				if (next && next.late < group.late) group.late = next.late;
				for (let i = index - 1; i >= 0 && this.#groups[i].late > group.late; i--)
					this.#groups[i].late = group.late;
				this.#groups.splice(index, 0, group);
				// Group overhead alone can fill the max buffer, past which next() moves on from a stalled head.
				if (this.#full()) {
					this.#notify?.();
					this.#notify = undefined;
				}

				this.#signals.spawn(this.#watch.bind(this, group));
			}
			this.#finish(null);
		} catch (err) {
			this.#finish(err instanceof Error ? err : new Error(String(err)));
		}
	}

	// Watch `group` for wire frames and its end, waking the reader. Only #look decodes, and the
	// watcher runs it at once rather than leave it to the next read, so buffered ranges, arrival times
	// and the age budget keep up while the reader is busy. Completion, faults and the pressure skip are
	// next()'s alone: it reads the transport's state as it stands, so nothing here settles a group
	// behind its back.
	async #watch(group: Group) {
		try {
			while (!this.#stopped(group)) {
				await group.consumer.readable();
				if (this.#stopped(group)) return;
				this.#look();
				this.#notify?.();
				this.#notify = undefined;
				if (group.fault || group.consumer.done) return;
				// A frame left unread behind a full max buffer waits for room or for the group to lead.
				if (!this.#admits(group)) await this.#park();
			}
		} finally {
			// However the watch ends, the reader looks at the group again: it may have ended or failed.
			this.#notify?.();
			this.#notify = undefined;
		}
	}

	// Take and decode every wire frame the transport already holds, oldest group first, each only while
	// the max buffer admits it, then skip what the new frames make too stale to wait for. The one place
	// frames come off the wire. Synchronous, so each admission check shares one step with the decode it
	// admits, and nothing can spend the same room twice or see a group half decoded.
	#look(): void {
		for (;;) {
			let decoded = false;
			// A frame past the cursor, which can show the groups ahead of it are too slow to wait for.
			let ahead = false;
			for (const group of this.#groups) {
				while (!group.fault && this.#admits(group)) {
					let wire: Moq.Group.Frame | undefined;
					try {
						wire = group.consumer.tryReadFrame();
					} catch {
						// The transport failed the group, such as on a slice that would not inflate. It now
						// reads as ended with that error, which next() takes as a cut tail.
						break;
					}
					if (!wire) break;
					this.#decode(group, wire);
					decoded = true;
					if (group.consumer.sequence !== this.#active) ahead = true;
				}
			}
			if (!decoded) return;
			this.#updateBuffered();
			// This runs even when the decoded group is the delivery head, because that is exactly the
			// stalled case (#active sits below every buffered group) where the max delay budget is what
			// eventually breaks the stall. A newer group reaching back to where the stalled active group
			// has already presented means it can advance now instead of waiting.
			if (!ahead) return;
			const aged = this.#checkMaxDelay();
			if (!this.#tryDurationSkip() && !aged) return;
			// The skip refunded room and moved the head, which may admit frames left unread.
		}
	}

	// Decode one wire frame into `group`, charging what it buffers. A payload that fails to decode sets
	// the group's fault, which next() raises once the frames ahead of it are read.
	#decode(group: Group, wire: Moq.Group.Frame): void {
		group.empty = false;
		// When the frame landed, not when it was decoded: a frame the max buffer left in the transport
		// would otherwise count the consumer's own wait as network jitter.
		const arrival = Nano.toMilli(Nano(wire.arrived ?? now()));

		// A malformed payload is not loss: it ends the reader where it sits, rather than truncating the group.
		let decoded: Frame[];
		try {
			decoded = this.#format.decode(wire.payload, wire.timestamp);
		} catch (err) {
			group.fault = err instanceof Error ? err : new Error(String(err));
			return;
		}
		if (this.#format.kind === "data") {
			// A data track's marker is one object with no bytes, alone in its group.
			if (group.bare || (decoded.length === 0 && group.media)) {
				group.fault = new Error("a data track's discontinuity marker shares its group");
				return;
			}
			group.bare = decoded.length === 0;
		}

		// Decoded payloads are views of the wire payload, which stays allocated until the last of them is
		// read, so that one also holds whatever of it the views leave out, such as a large header.
		let views = 0;
		for (const sample of decoded) {
			const marker = this.#format.end?.(sample) !== undefined;

			// Observed here, before any gap handling or age budget: a target derived from
			// what survives the budget would only ever confirm the budget it was cut to.
			if (!marker) {
				this.#jitter.observe(arrival, Milli.fromMicro(sample.timestamp));
				this.#spread.set(this.#jitter.measured);
			}
			const frame: ReceivedFrame = {
				arrival,
				payload: sample.payload,
				timestamp: sample.timestamp,
				// Protocol invariant: groups always start at a keyframe.
				// For index 0, we enforce this regardless of what the format reports.
				// For index > 0, we trust the format's keyframe detection.
				keyframe: !marker && group.index === 0 ? true : sample.keyframe,
				// Carry the container's per-sample duration through so group.end is the real
				// presentation end (ts + duration), not just the last frame's ts. This is what
				// makes the PTS-contiguity check (next.firstPTS <= group.end) work; without it a
				// contiguous next group looks one frame past the end. Undefined for Legacy (no duration).
				duration: sample.duration,
			};

			if (!marker) {
				group.index++;
				group.media = true;
				if (group.minMedia === undefined || frame.timestamp < group.minMedia) {
					group.minMedia = frame.timestamp;
				}
			}

			group.start ??= frame.timestamp;
			group.frames.push(frame);
			const cost = FRAME_COST + frame.payload.byteLength;
			group.costs.push(cost);
			this.#charge(group, cost);
			views += frame.payload.byteLength;

			if (group.latest === undefined || frame.timestamp > group.latest) {
				group.latest = frame.timestamp;
			}

			const end = (frame.timestamp + (frame.duration ?? 0)) as Micro;
			if (group.end === undefined || end > group.end) {
				group.end = end;
			}
		}
		const rest = wire.payload.byteLength - views;
		if (decoded.length > 0 && rest > 0) {
			group.costs[group.costs.length - 1] += rest;
			this.#charge(group, rest);
		}
	}

	// The transport's verdict on a group with nothing left to read: null once it finished cleanly, the
	// error that cut its tail off, or undefined while more may arrive.
	#ended(group: Group): Error | null | undefined {
		if (!group.consumer.done) return undefined;
		return group.consumer.closed.peek() ?? null;
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

	// Skip the oldest groups while the buffered span exceeds the max delay. Returns true if any was skipped.
	#checkMaxDelay(): boolean {
		if (this.#active === undefined) return false;

		let skipped = 0;
		const start = this.#groups[0]?.consumer.sequence;

		// Keep skipping the oldest group while the buffered span exceeds the max delay.
		// This also handles gaps in group sequence numbers: if #active points to a missing
		// group, the span proves the missing content is too old to wait for.
		while (this.#groups.length >= 2) {
			const threshold = Micro.fromMilli(this.#maxDelay.peek());
			const first = this.#groups[0];

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

			const age = max - min;
			if (age <= threshold) break;

			this.#groups.shift();
			this.#active = this.#groups[0]?.consumer.sequence;

			const next = this.#groups[0];
			const nextStart = next?.frames.at(0)?.timestamp ?? next?.end;
			const marker = !first.empty && !first.media;
			// Unread media is thrown away, and `first.end` counts it, so contiguity alone would hide it.
			// A trailing endpoint carries no media, so leaving it unread discards nothing.
			const discarded = first.frames.some((frame) => this.#format.end?.(frame) === undefined);
			// Adjacency proves nothing is missing only once the group finished cleanly: a truncated
			// or still-open group may have lost its tail.
			const ended = this.#ended(first);
			const truncated = ended instanceof Error;
			const adjacent = next?.consumer.sequence === first.consumer.sequence + 1;
			const continued = (ended === null && adjacent) || ptsContiguous(first.end ?? this.#presentedEnd, nextStart);
			if (marker || discarded || truncated || !continued) this.#lost = true;
			skipped++;
			if (!this.#discard(first)) break;
		}

		if (skipped) {
			console.warn(
				`skipping slow groups: track=${this.#track.name} ${start} -> ${this.#active} count=${skipped}`,
			);
			this.#updateBuffered();
		}
		return skipped > 0;
	}

	// Skip the stalled active group once it has presented up to where the next group
	// begins (its furthest frame end reaches the next group's first timestamp). Only
	// fires when the active group is fully consumed and still open, so we never drop
	// frames the consumer hasn't seen. Returns true if a group was skipped.
	#tryDurationSkip(): boolean {
		if (this.#active === undefined) return false;

		const active = this.#groups[0];
		if (!active || active.consumer.sequence !== this.#active) return false;
		if (this.#ended(active) !== undefined || active.frames.length > 0 || active.end === undefined) return false;

		const next = this.#groups[1];
		const nextStart = next?.frames.at(0)?.timestamp;
		if (!next || nextStart === undefined || active.end < nextStart) return false;

		this.#groups.shift();
		console.warn(`skipping covered group: ${active.consumer.sequence} -> ${next.consumer.sequence}`);
		this.#recordPresented(active);
		this.#active = next.consumer.sequence;

		this.#discard(active);
		this.#updateBuffered();
		return true;
	}

	// Move on from the stalled head once the max buffer is full, since it can hold no more of what it
	// waits behind, to the next group even with nothing of it read yet: that one reads as the head, and
	// each such skip refunds what the group left behind held. The head may be the active group or a
	// backlog group admitted below it. Only fires when the head is fully consumed and still open, so we
	// never drop frames the consumer hasn't seen: #look already read what the transport holds for it.
	// Returns true if it skipped a group.
	#tryPressureSkip(): boolean {
		if (this.#active === undefined || !this.#full()) return false;

		const head = this.#groups[0];
		if (!head || head.consumer.sequence > this.#active) return false;
		if (head.fault || head.frames.length > 0 || this.#ended(head) !== undefined) return false;

		const next = this.#groups[1];
		if (!next) return false;

		this.#groups.shift();
		console.warn(
			`skipping a stalled group past the max buffer: track=${this.#track.name} ${head.consumer.sequence} -> ${next.consumer.sequence}`,
		);
		if (head.consumer.sequence === this.#active) {
			// Still open, so its tail is lost unless the next group picks up where it ended, which nothing
			// proves before its start is read. A marker group stops the timeline, so moving past one loses it.
			const nextStart = next.frames.at(0)?.timestamp;
			const marker = !head.empty && !head.media;
			if (marker || !ptsContiguous(head.end ?? this.#presentedEnd, nextStart)) this.#lost = true;
			this.#recordPresented(head);
			this.#active = next.consumer.sequence;
		} else {
			// A backlog group below the cursor, which stays put: what the group would still have delivered is lost.
			this.#lost = true;
		}

		this.#discard(head);
		this.#updateBuffered();
		return true;
	}

	#full(): boolean {
		return this.#used >= this.#maxBuffer;
	}

	#charge(group: Group, bytes: number): void {
		group.charged += bytes;
		this.#used += bytes;
	}

	#refund(group: Group, bytes: number): void {
		group.charged -= bytes;
		this.#used -= bytes;
		this.#wake();
	}

	// Refund everything a group dropped from #groups still holds.
	#release(group: Group): void {
		if (group.released) return;
		group.released = true;
		this.#refund(group, group.charged);
	}

	// Whether `group` may decode another wire frame: the max buffer has room, or it is the head with
	// nothing buffered, so reading never waits on its own lookahead.
	#admits(group: Group): boolean {
		return !this.#full() || (group === this.#groups[0] && group.frames.length === 0);
	}

	// Whether `group`'s watcher should stop: the group was dropped, so what it decodes would never be
	// refunded, or the consumer closed.
	#stopped(group: Group): boolean {
		return group.released === true || this.#signals.abort.aborted;
	}

	// Park until something may have freed room or moved the head; see #wake.
	#park(): Promise<void> {
		return new Promise<void>((resolve) => this.#room.push(resolve));
	}

	// Let parked watchers re-check the max buffer: something was refunded, or the head or cursor moved.
	#wake(): void {
		const room = this.#room;
		this.#room = [];
		for (const resolve of room) resolve();
	}

	// Drop a group without delivering it. A drop must not hide what was already found, so one holding
	// a fault, or with frames below the latest delivered group start, ends the reader instead. What
	// #look has not decoded yet stays unchecked: decoding data only to throw it away is work a
	// peer could force. Returns false if it ended the reader.
	#discard(group: Group): boolean {
		group.consumer.close();
		group.frames.length = 0;
		group.costs.length = 0;
		this.#release(group);
		const start = this.#start;
		if (
			start !== undefined &&
			group.consumer.sequence > start.group &&
			group.minMedia !== undefined &&
			group.minMedia < start.timestamp
		) {
			this.#abort(new Error("group timestamp is below the previous group start"));
		} else if (group.fault) {
			this.#abort(group.fault);
		}
		return this.#error === undefined;
	}

	// The first error ends the reader; nothing after it in delivery order replaces it.
	#abort(error: Error): void {
		if (this.#error) return;
		this.#error = error;
		this.#finish(error);
		this.#track.close(error);
		this.#notify?.();
		this.#notify = undefined;
		// Nothing more is read, so stop the group watchers and release their groups rather than let a
		// peer keep them decoding and buffering. After the wake: next() reads a closed effect as closed.
		this.#signals.close();
	}

	/**
	 * Returns the next {@link Read} in delivery order, awaiting one if needed: a frame, a gap where
	 * media after an earlier item will never arrive, or an end the publisher declared.
	 * An unmarked forward timestamp jump is not a gap, since nothing on the wire says media is missing.
	 * Errors come in delivery order: a payload that fails to decode throws after the items ahead of it,
	 * and skipping a malformed group throws rather than reading as a gap. After buffered groups drain,
	 * a finished track returns undefined and an aborted track throws. Only a reset or evicted group reads
	 * as a gap.
	 */
	async next(): Promise<Read | undefined> {
		if (this.#next) {
			const next = this.#next;
			this.#next = undefined;
			return next;
		}
		for (;;) {
			if (this.#error) throw this.#error;
			// Decode what has arrived, so every decision below sees it.
			this.#look();
			// Its skips can raise a fault, which a finished track's settled `#closed` cannot carry.
			if (this.#error) throw this.#error;

			const ended = this.#closed.peek();
			if (this.#groups[0]?.consumer.sequence === this.#active || this.#groups.length === 0) {
				this.#hole = undefined;
			}
			if (this.#groups.length === 0) {
				if (ended !== undefined) {
					if (ended instanceof Error) throw ended;
					return undefined;
				}
			}

			// If #active points below all buffered groups -- e.g. a finished group's `+ 1`
			// fallback fired because no later group was buffered yet, and the real (large-gap,
			// non-sequential) next group has since arrived -- promote #active to the first real
			// group so delivery resumes instead of stalling on a nonexistent sequence.
			// Before any media is delivered there is no timeline hole to wait for, so this exception
			// only applies at startup.
			// Otherwise promote #active when the first buffered group continues the timeline we left off at,
			// when a completed empty group can be walked (empty groups mean nothing), or when the hole
			// is proven: the head already reaches past where presentation left off by more than the
			// max delay, so anything still missing in between would arrive too old to play. Proving it
			// here matters: #checkMaxDelay would instead drop the head, the very group to play next.
			// After track termination no missing group can arrive, so drain across any remaining gap.
			// A missing sequence also expires on elapsed monotonic time, so timestamp-free markers
			// behind a hole can end a paused track without more arrivals. It is late since a group
			// past it arrived, not since the cursor got here: once the edge stops moving, a backlog
			// with every other group missing would otherwise drain one budget per hole.
			if (this.#active !== undefined && this.#groups.length > 0) {
				const head = this.#groups[0];
				if (head.consumer.sequence > this.#active) {
					this.#hole = head.late;
					const startup = this.#start === undefined;
					const contiguous = ptsContiguous(this.#presentedEnd, head.frames.at(0)?.timestamp);
					const empty = head.empty && head.consumer.done;
					const maxDelay = Micro.fromMilli(this.#maxDelay.peek());
					const skipHole =
						head.frames.length > 0 &&
						(maxDelay === 0 ||
							(this.#presentedEnd !== undefined &&
								head.latest !== undefined &&
								head.latest - this.#presentedEnd > maxDelay));
					const elapsed = Instant.durationSince(now(), this.#hole) >= Nano.fromMicro(maxDelay);
					// The groups past the hole already fill the max buffer, so it can hold none of what would arrive into it.
					const full = this.#full();
					if (startup || empty || contiguous || skipHole || elapsed || full || ended !== undefined) {
						if (!contiguous && !empty) {
							this.#lost = true;
							if (!startup) {
								console.warn(
									`skipping missing groups: track=${this.#track.name} ${this.#active} -> ${head.consumer.sequence}`,
								);
							}
						}
						this.#active = head.consumer.sequence;
						this.#hole = undefined;
					}
				}
			}

			const head = this.#groups.at(0);
			if (head && this.#active !== undefined && head.consumer.sequence <= this.#active) {
				const seq = head.consumer.sequence;
				const frame = head.frames.shift();
				if (frame) {
					this.#refund(head, head.costs.shift() ?? 0);
					const end = this.#format.end?.(frame);
					if (end !== undefined) {
						this.#updateBuffered();
						// A trailing endpoint is the last frame's duration: it moves the edge, nothing else.
						if (seq === this.#mediaGroup) {
							this.#advance(end);
							continue;
						}
						return this.#emit({ kind: "end", group: seq, end });
					}
					this.#mediaGroup = seq;

					if (this.#start === undefined || seq > this.#start.group) {
						this.#floor = this.#start?.timestamp;
						this.#start = { group: seq, timestamp: frame.timestamp };
					}
					// Delayed history has its own earlier floor; this bound belongs to #start.group.
					if (seq === this.#start.group && this.#floor !== undefined && frame.timestamp < this.#floor) {
						this.#abort(new Error("frame timestamp is below the previous group start"));
						throw this.#error;
					}
					this.#updateBuffered();
					return this.#emit({ kind: "frame", group: seq, frame });
				}

				// A payload that failed to decode ends the reader once the frames ahead of it are read.
				if (head.fault) {
					this.#abort(head.fault);
					throw head.fault;
				}

				// Remove the group once the transport has settled it and every frame is read. A
				// below-#active group (a backlog group admitted behind the live edge) may still be
				// downloading when its buffer momentarily drains, and removing it then silently
				// truncates its tail. Its watcher wakes this read whenever the group gains a frame or
				// ends, and #checkMaxDelay bounds how long a stalled head can hold delivery up.
				const settled = this.#ended(head);
				if (settled !== undefined) {
					this.#groups.shift();
					if (seq === this.#active) {
						// Advance to the next buffered group's actual sequence, but ONLY if it continues this
						// group's timeline. Some encoders number groups non-sequentially with large gaps (not
						// +1), so a bare `+= 1` would point #active at a nonexistent sequence and stall until
						// the hole expired -- every group through the skip path, i.e. constant stutter. A real
						// PTS gap is different: an intermediate group may still be in transit, so fall back to
						// +1 there (the promotion guard above fixes it up once a continuous group arrives).
						this.#recordPresented(head);
						const next = this.#groups.at(0);
						this.#active = continues(head, next) ? next.consumer.sequence : seq + 1;
					}
					head.consumer.close();
					this.#release(head);
					const truncated = settled instanceof Error;
					if (truncated) this.#lost = true;
					this.#updateBuffered();
					// A data track's marker carries no timestamp, so it ends where this reader got to.
					// Before the first item there is nothing to end.
					if (head.bare && !truncated && this.#edge !== undefined) {
						return this.#emit({ kind: "end", group: seq, end: this.#edge });
					}
					continue;
				}

				// The active group is stalled with nothing buffered. If a later group
				// has already been reached by this group's duration, skip ahead now
				// rather than waiting for the stalled group to resume.
				if (this.#tryDurationSkip()) continue;
				if (this.#tryPressureSkip()) continue;
			}

			if (this.#notify) {
				throw new Error("multiple calls to next not supported");
			}
			// The head or cursor may have moved, which can exempt a parked watcher from the max buffer.
			this.#wake();

			const abort = this.#signals.abort;
			if (abort.aborted) return undefined;
			let timer: Effect | undefined;
			if (this.#hole !== undefined) {
				timer = new Effect();
				const since = this.#hole;
				timer.run((effect) => {
					const budget = Nano.fromMilli(effect.get(this.#maxDelay));
					const elapsed = Instant.durationSince(now(), since);
					effect.timer(() => this.#notify?.(), Nano(Math.max(0, budget - elapsed)));
				});
			}

			const aborted = await new Promise<boolean>((resolve) => {
				const onAbort = () => resolve(true);
				abort.addEventListener("abort", onAbort, { once: true });
				this.#notify = () => {
					abort.removeEventListener("abort", onAbort);
					resolve(false);
				};
			});
			timer?.close();

			this.#notify = undefined;
			if (aborted) return undefined;
		}
	}

	// Return `read`, or a gap ahead of it when media went missing after an earlier item.
	#emit(read: Exclude<Read, { kind: "gap" }>): Read {
		const resume = read.kind === "frame" ? read.frame.timestamp : read.end;
		const edge = this.#edge;
		const lost = this.#lost;
		this.#lost = false;
		this.#advance(read.kind === "frame" ? Micro(resume + (read.frame.duration ?? Micro.zero)) : resume);
		if (!lost || edge === undefined) return read;

		// A next group may start before the edge (an overlapping endpoint), so the span bottoms out at zero.
		const timestamp = Micro(Math.min(edge, resume));
		this.#next = read;
		return { kind: "gap", timestamp, duration: Micro(resume - timestamp) };
	}

	#advance(end: Micro): void {
		if (this.#edge === undefined || end > this.#edge) this.#edge = end;
	}

	#updateBuffered(): void {
		const ranges: BufferedRanges = [];

		let prev: Group | undefined;

		for (const group of this.#groups) {
			const first = group.frames.at(0);
			if (!first || group.latest === undefined) continue;

			const start = Milli.fromMicro(first.timestamp);
			const end = Milli.fromMicro(group.latest);

			const last = ranges.at(-1);
			const contiguous = prev?.consumer.done && prev.consumer.sequence + 1 === group.consumer.sequence;
			if (last && (last.end >= start || contiguous)) {
				last.end = Milli(Math.max(last.end, end));
			} else {
				ranges.push({ start, end });
			}

			prev = group;
		}

		this.#buffered.set(ranges);
	}

	/** Stop consuming and release the track and all buffered groups. */
	close(): void {
		// The item a gap was returned ahead of is buffered media too.
		this.#next = undefined;
		this.#finish(null);
		this.#signals.close();
	}
}
