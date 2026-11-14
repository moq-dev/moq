import { Duration, Instant, now } from "@moq/clock";
import type * as Moq from "@moq/net";
import { Effect, type Getter, type GetterInit, getter, Once, Signal } from "@moq/signals";
import type { Format } from "./format";
import { Jitter } from "./jitter";
import type { BufferedRanges, Frame, ReceivedFrame } from "./types";

/**
 * The longest playout delay: it caps a reader's latency and how far catalog `warmup` reaches back,
 * so a publisher cannot hold a reader's media without bound. Mirrors `moq_mux::container::DELAY_MAX`.
 */
export const DELAY_MAX = Duration.secs(10);

/** Options for constructing a {@link Consumer}. */
export interface ConsumerProps {
	/** The container format used to decode each MoQ frame. */
	format: Format;
	/**
	 * How far behind the live edge to play (default: zero for both bounds). See `Moq.Track.Delay`.
	 *
	 * The start is the group covering the live edge less `min`, plus any {@link ConsumerProps.warmup}, waiting
	 * for it when the newest group arrives first; an unstated (`null`) `min` reads as zero. A `min`
	 * past `max` starts at `max`, since the publisher serves nothing older. `max` is measured as
	 * the span from the oldest buffered frame to the newest, so it bounds how long a late group is
	 * waited for, and a missing one unless {@link ConsumerProps.wait} is shorter. It is the local
	 * half of the subscription's `delay.max`; both measure the same budget, one on the wire and one
	 * as frames are read. It bounds a join's lateness too: a backlog that landed before the first
	 * read starts at the first group whose first frame is within it, plus any warmup, of the
	 * newest content, though the start keeps the group covering the live edge less `min` and the
	 * warmup.
	 */
	// Read-only: a Getter (e.g. another component's output) is accepted directly.
	delay?: GetterInit<Moq.Track.Delay>;
	/**
	 * How long a missing group is waited for before the consumer skips past it, at most `delay.max`
	 * (default: `delay.max`). It expires this much monotonic time after the first group past it
	 * arrived, even when that group is a marker on a paused track, or sooner once where that group
	 * begins falls `delay.max` behind the newest content. Groups that did arrive still play until they age past
	 * `delay.max`, so a player can wait on holes for its playout delay alone while still receiving
	 * the late media it catches up on.
	 */
	// Read-only: a Getter (e.g. another component's output) is accepted directly.
	wait?: GetterInit<Duration | undefined>;
	/**
	 * Cap the bytes of decoded frames held ahead of the reader, counting the whole wire payload they
	 * share and each frame's and group's own overhead (default: one group at the transport's size
	 * cap). Mirrors Rust's `Consumer::set_max_buffer`.
	 *
	 * Once it is full the consumer stops decoding ahead and stops pulling new groups past one successor
	 * to the group being read, so the undecoded rest stays in the transport's cache, which bounds it. The group being read still decodes a wire
	 * frame whenever it holds nothing, so reading never waits on its own lookahead. If it stalls while the
	 * cap is full, the consumer moves on to the next group like a skip past a slow group, read or not: no
	 * more of what it waits behind can be held.
	 *
	 * @internal
	 */
	maxBuffer?: number;
	/**
	 * How much further than the min delay the start reaches, in milliseconds (default: 0), so a
	 * decoder that needs that much media before its output is clean starts early enough, such as an
	 * Opus decoder or the max-delay decoder of an intra-refresh rendition. Pass the catalog
	 * rendition's `warmup`. Mirrors Rust's `container::Playout`.
	 *
	 * Put the max delay plus the warmup on the subscription, since the publisher skips what falls
	 * outside that budget. The start is no later than the group covering the live edge less
	 * `delay.min` and the warmup: one that starts at or before that point. Both default to zero,
	 * which the newest group covers, so the start waits for nothing. Otherwise groups can arrive
	 * newest first, so when none that arrived covers that point the start waits for an earlier one,
	 * until one covers it, group 0 arrived, the track finishes, the max buffer fills, or the newest
	 * group's arrival is that whole reach ago. The wall clock bounds a quiet or paused track, which
	 * never ages out in media time. Delivery then starts at the lowest group that arrived. Until the
	 * reader is the warmup past both its first frame and that point, the age skip keeps a group that
	 * covers the reach and tolerates the warmup, so the media leading up to the start is not dropped
	 * as stale. After that, the max delay alone governs it.
	 */
	warmup?: Duration;
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
	| { kind: "frame"; group: bigint; frame: ReceivedFrame }
	/**
	 * Media after an item already read will never reach this reader: a group was skipped, evicted,
	 * truncated, or never arrived. A decoder resumes without the references or state that media
	 * built. Joining mid-track is not a gap: nothing was read before it.
	 *
	 * The span is an upper bound. `timestamp` is the furthest point already read; without a
	 * per-frame duration that is the last frame's timestamp, one frame early. `duration` runs to
	 * where the next item resumes, so it also covers any declared pause in between.
	 */
	| { kind: "gap"; timestamp: Duration; duration: Duration }
	/**
	 * The publisher declared that its media stops at `end`: it paused, switched source, or finished.
	 * Frames that follow from the same `group` are the stopped encoder's final packets (an audio
	 * codec's drain), whose output at or past `end` is padding. The next group starts fresh.
	 */
	| { kind: "end"; group: bigint; end: Duration };

// The default max buffer: one group at the transport's size cap. Mirrors Rust's.
const DEFAULT_MAX_BUFFER = 32 * 1024 * 1024;

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
	// Timestamps in microseconds, kept as numbers since every frame updates them.
	start?: number; // First decoded timestamp
	minMedia?: number; // Lowest decodable timestamp
	latest?: number; // The timestamp of the latest known frame
	end?: number; // The furthest presentation point so far, i.e. max(timestamp + duration)
	fault?: Error; // The payload that failed to decode, an error after the buffered frames are delivered.
	index: number; // Media frames decoded so far: the first is a keyframe by protocol invariant
	charged: number; // What this group holds against the max buffer
	released?: boolean; // Dropped from #groups, with what it held refunded
}

// The earliest and latest known frame timestamps of the buffered groups from each index on.
interface Spans {
	min: (number | undefined)[];
	max: (number | undefined)[];
}

// Two adjacent groups are treated as timeline-contiguous when the next group's first PTS is within
// this slack of the current group's end. Per-sample durations and base-decode-times are each rounded
// to microseconds independently, so a genuinely contiguous boundary can be off by ~1µs (seen on 48kHz
// audio). A real missing group spans ~one group duration (orders of magnitude larger), so 1ms cleanly
// separates rounding noise from an actual gap.
const CONTIGUITY_TOLERANCE = 1000;

/**
 * True when `nextStart` continues the timeline that ends at `end`: it lands at or before `end`,
 * within CONTIGUITY_TOLERANCE to absorb the µs rounding of independently-rounded per-sample
 * durations and base-decode-times. Undefined on either side means continuity can't be proven.
 *
 * The bound is one-sided (upper only) by design: a next start at or before `end` continues the
 * timeline, a start past `end` beyond the tolerance is a gap. Group starts are validated
 * separately; an overlapping endpoint is not a rewind.
 */
function ptsContiguous(end: number | undefined, nextStart: number | undefined): boolean {
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
		next.consumer.sequence === prev.consumer.sequence + 1n ||
		ptsContiguous(prev.end, next.frames.at(0)?.timestamp.asMicros())
	);
}

// The max delay in microseconds, infinite when unlimited.
function maxMicros(delay: Moq.Track.Delay): number {
	return delay.max === null ? Number.POSITIVE_INFINITY : (delay.max?.asMicros() ?? 0);
}

/**
 * Reads frames from a MoQ track in group order, buffering groups and skipping ones that fall the max
 * delay behind. Delivery starts at the floor the subscription's ranges name, raised to where the
 * publisher reports its feed starts, or with neither at the first group to arrive; a group that arrives
 * below the cursor is never delivered. Read in arrival order from the track itself to keep every group.
 */
export class Consumer {
	#track: Moq.Track.Subscriber;
	#format: Format;
	#delay: Getter<Moq.Track.Delay>;
	#wait: Getter<Duration | undefined>;
	#maxBuffer: number;
	// How much further than the min delay the start reaches, and the age skip tolerates while warming.
	#warmup: Duration;
	// The start may still wait for a group covering the reach, until that wait ends for good.
	#starting = true;
	// When the newest group had landed as the first one arrived, once the start waits.
	#anchor?: Instant;
	// The point the start reached back to, the live edge less the reach, once its wait ended.
	#target?: number;
	// Where the warming ends: the warmup past the first frame read, or past the start's target when
	// that is later. Undefined before the first frame.
	#warmed?: number;
	// What the buffered groups hold, against the max buffer.
	#used = 0;
	// Group watchers and the track loop parked on a full max buffer.
	#room: (() => void)[] = [];
	#groups: Group[] = [];
	#active?: bigint; // the active group sequence number
	// The cursor started at the floor or the publisher's resolved start, rather than at the first
	// arrival, so a hole at it is waited for like any other.
	#floored = false;
	// When the missing sequence at the cursor became late, while one is.
	#hole?: Instant;
	// Presentation end (max PTS + duration) of the group we most recently advanced past, so next()'s
	// promotion guard can tell a timeline-continuous next group from one sitting after a gap.
	// Written only by #pass, which every move past a group goes through.
	#presentedEnd?: number;
	// Group of the last media frame next() returned, so an endpoint after it reads as that frame's
	// duration rather than a declared stop.
	#mediaGroup?: bigint;
	// Media went missing since the last item read, so the next one is preceded by a gap.
	#lost = false;
	// The furthest presentation point read so far: max(timestamp + duration), endpoints included.
	// Undefined until the first item, so a loss before it is no gap.
	#edge?: number;
	// The item a gap was returned ahead of.
	#next?: Read;
	// The first media timestamp of the latest delivered group, and its predecessor's start.
	// Endpoints and later frames can overlap the next group without moving this floor.
	#start?: { group: bigint; timestamp: number };
	#floor?: number;
	// The error that ended the reader, the first in delivery order: a decode failure or a rewind.
	#error?: Error;

	// Wake up the consumer when a new frame is available.
	#notify?: () => void;

	#buffered = new Signal<BufferedRanges>([]);
	/** The time ranges currently buffered and ready to play. */
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	// The transport delivered every group up to the publisher's declared end.
	#delivered = false;
	#finished = new Signal(false);
	/**
	 * Whether the publisher finished the track and every group up to its end has arrived and ended,
	 * so {@link buffered} holds all the media left to read. Unlike {@link next} returning undefined,
	 * it does not wait for the reader to drain what is buffered. Never set by {@link close}.
	 */
	readonly finished: Getter<boolean> = this.#finished;

	#jitter: Jitter;
	#spread: Signal<Duration>;
	/**
	 * How much buffer late arrivals need, measured as frames come off the transport.
	 *
	 * The measured term of doc/concept/audio-jitter.md, excluding the codec frame and any advertised
	 * floor. Starts at the predecessor's estimate, or the cold-start estimate without one.
	 */
	readonly spread: Getter<Duration>;
	// The newest group opened by a declared end, until a later group's first frame tells the jitter
	// estimator the silence is over. Not at the marker itself: the stopped encoder's final packets may
	// follow it in its own group, and they belong to the speech before the silence.
	#declared?: bigint;
	#mediaSequence?: bigint;

	#signals = new Effect();
	#closed = new Once<Error | null>();

	/** Start consuming the given track, decoding frames with `props.format`. */
	constructor(track: Moq.Track.Subscriber, props: ConsumerProps) {
		this.#track = track;
		this.#format = props.format;
		this.#delay = getter(props.delay ?? {});
		this.#wait = getter(props.wait);
		this.#maxBuffer = props.maxBuffer ?? DEFAULT_MAX_BUFFER;
		this.#warmup = (props.warmup ?? Duration.ZERO).min(DELAY_MAX);
		this.#jitter = props.predecessor ? props.predecessor.#jitter.fork() : new Jitter();
		this.#spread = new Signal(Duration.millis(this.#jitter.measured));
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

				// Delivery starts at the floor the ranges name, raised to where the publisher says its feed
				// starts: it serves nothing below, and resolved a relative start against its own latest
				// group, which may hold no frame yet and so count in no floor resolved here. Groups go out
				// newest first, so the first arrival is not the oldest served; without either it stands
				// in. Mirrors Rust's `Consumer::poll_read`.
				if (this.#active === undefined) {
					const floor = this.#track.floor();
					const served = await this.#track.start();
					const start = served !== undefined && (floor === undefined || served > floor) ? served : floor;
					this.#active = this.#startAt(start) ?? consumer.sequence;
				}

				// A group below the cursor is never delivered: what lies below is already covered by the
				// gap that moved the cursor, or was never read. Mirrors Rust's consumer. While the start
				// still waits for a group covering its reach, it starts at the lowest that arrived.
				if (!this.#starting && consumer.sequence < this.#active) {
					consumer.close();
					continue;
				}
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
				if (next?.late.lt(group.late)) group.late = next.late;
				for (let i = index - 1; i >= 0 && this.#groups[i].late.gt(group.late); i--)
					this.#groups[i].late = group.late;
				this.#groups.splice(index, 0, group);
				// Group overhead alone can fill the max buffer, past which next() moves on from a stalled head.
				if (this.#full()) {
					this.#notify?.();
					this.#notify = undefined;
				}

				this.#signals.spawn(this.#watch.bind(this, group));
			}
			if (!this.#signals.abort.aborted) {
				this.#delivered = true;
				this.#settle();
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
			this.#settle();
		}
	}

	// Mark the track finished once the transport delivered its last group and every buffered group ended.
	#settle(): void {
		if (!this.#delivered || this.#finished.peek()) return;
		if (this.#groups.every((group) => group.fault !== undefined || group.consumer.done)) this.#finished.set(true);
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

	// Start the cursor at `start`, when there is one, rather than at the first arrival: the start no
	// longer waits for a group covering the reach, since nothing below the start arrives.
	#startAt(start: bigint | undefined): bigint | undefined {
		if (start === undefined) return undefined;
		this.#active = start;
		this.#floored = true;
		this.#starting = false;
		return start;
	}

	// Note an end the publisher declared in `group`, which a later group's first frame resumes from.
	#declare(group: Group): void {
		const sequence = group.consumer.sequence;
		if (this.#mediaSequence !== undefined && sequence < this.#mediaSequence) return;
		if (this.#declared === undefined || sequence > this.#declared) this.#declared = sequence;
	}

	// Decode one wire frame into `group`, charging what it buffers. A payload that fails to decode sets
	// the group's fault, which next() raises once the frames ahead of it are read.
	#decode(group: Group, wire: Moq.Group.Frame): void {
		group.empty = false;
		// When the frame landed, not when it was decoded: a frame the max buffer left in the transport
		// would otherwise count the consumer's own wait as network jitter.
		const arrival = wire.arrived ?? now();
		const arrived = arrival.since(Instant.ZERO).asMillis();

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
			if (group.bare) this.#declare(group);
		}

		// Decoded payloads are views of the wire payload, which stays allocated until the last of them is
		// read, so that one also holds whatever of it the views leave out, such as a large header.
		let views = 0;
		for (const sample of decoded) {
			const marker = this.#format.end?.(sample) !== undefined;

			// Observed here, before any gap handling or age budget: a target derived from
			// what survives the budget would only ever confirm the budget it was cut to.
			if (!marker) {
				if (this.#mediaSequence === undefined || group.consumer.sequence > this.#mediaSequence)
					this.#mediaSequence = group.consumer.sequence;
				if (this.#declared !== undefined && group.consumer.sequence > this.#declared) {
					this.#jitter.end();
					this.#declared = undefined;
				}
				this.#jitter.observe(arrived, sample.timestamp.asMillis());
				const spread = this.#jitter.measured;
				if (spread !== this.#spread.peek().asMillis()) this.#spread.set(Duration.millis(spread));
			} else if (!group.media) {
				// Only a marker ahead of any media declares an end; one after it is a frame's endpoint.
				this.#declare(group);
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

			const at = frame.timestamp.asMicros();
			if (!marker) {
				group.index++;
				group.media = true;
				if (group.minMedia === undefined || at < group.minMedia) {
					group.minMedia = at;
				}
			}

			group.start ??= at;
			group.frames.push(frame);
			const cost = FRAME_COST + frame.payload.byteLength;
			group.costs.push(cost);
			this.#charge(group, cost);
			views += frame.payload.byteLength;

			if (group.latest === undefined || at > group.latest) {
				group.latest = at;
			}

			const end = at + (frame.duration?.asMicros() ?? 0);
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

	// Move the cursor past `group` to `next`, recording where the group's content ends. next()'s
	// promotion guard compares the following group's first PTS against that to tell an unbroken
	// timeline from a real gap. A group with no frames (empty, or errored before the first one) says
	// nothing about the timeline, so it leaves the last known end in place rather than wiping it.
	#pass(group: Group, next: bigint | undefined): void {
		if (group.end !== undefined) this.#presentedEnd = group.end;
		this.#active = next;
	}

	// Skip the oldest groups while the buffered span exceeds the max delay. Returns true if any was skipped.
	#checkMaxDelay(): boolean {
		if (this.#active === undefined) return false;

		let skipped = 0;
		const start = this.#groups[0]?.consumer.sequence;
		// The span left after each further skip, computed once the first skip shows more may follow.
		let spans: Spans | undefined;
		// The index of the first stamped group past the head, kept across skips so a run of unstamped
		// groups ahead of it stays linear: each skip shifts it down one, and only a skip onto it rescans.
		let stamped = 0;

		// Keep skipping the oldest group while the buffered span exceeds the max delay.
		// This also handles gaps in group sequence numbers: if #active points to a missing
		// group, the span proves the missing content is too old to wait for.
		while (this.#groups.length >= 2) {
			const threshold = this.#ageBudget();
			const first = this.#groups[0];

			// Check the difference between the earliest and latest known frames.
			let min: number | undefined;
			let max: number | undefined;

			if (skipped === 0) {
				for (const group of this.#groups) {
					if (group.latest === undefined) continue;

					const frame = group.frames.at(0)?.timestamp.asMicros() ?? group.latest;
					if (min === undefined || frame < min) min = frame;
					if (max === undefined || group.latest > max) max = group.latest;
				}
			} else {
				// A skip removes only the oldest group, so one pass over what the first skip left gives
				// the span after every later one, and a bulk skip stays linear in the buffered groups.
				spans ??= this.#spans();
				min = spans.min[skipped - 1];
				max = spans.max[skipped - 1];
			}

			if (min === undefined || max === undefined) break;

			const age = max - min;
			if (age <= threshold) break;
			if (this.#edge === undefined) {
				// Until the reader warms, a group may cover the start's reach though it began before it.
				// Before the first item, a backlog that landed whole starts within the budget, so only
				// the group covering the live edge less the reach is kept: one whose successor starts
				// past that point. Rust's start skip, in `Consumer::poll_event_once`, measures the same.
				// `max` is the live edge of the groups left, so this stays O(1) per skip.
				if (this.#warming()) {
					const successor = this.#groups[1].start;
					if (successor === undefined) break;
					const target = this.#startTarget(this.#reach(this.#delay.peek()), max);
					if (target !== undefined && successor > target) break;
				}
			} else {
				// Once reading began, the head is stale only once the first stamped group past it starts a
				// whole budget behind the newest content: everything the head and any group between could
				// still present ends there. Its own first frame is no bound, since the skip moves the cursor
				// past every missing group in between, which a group arriving below it then never delivers.
				// An unstamped group between ages out on a later pass, as the head. Mirrors Rust's skip of
				// a blocking group, which measures from `poll_next_group`.
				if (stamped < 1) stamped = this.#groups.findIndex((group, i) => i > 0 && group.start !== undefined);
				const successor = stamped < 1 ? undefined : this.#groups[stamped].start;
				if (successor === undefined || max - successor < threshold) break;
			}

			this.#groups.shift();
			stamped--;
			this.#pass(first, this.#groups[0]?.consumer.sequence);

			const next = this.#groups[0];
			const nextStart = next?.frames.at(0)?.timestamp.asMicros() ?? next?.end;
			const marker = !first.empty && !first.media;
			// Unread media is thrown away, and `first.end` counts it, so contiguity alone would hide it.
			// A trailing endpoint carries no media, so leaving it unread discards nothing.
			const discarded = first.frames.some((frame) => this.#format.end?.(frame) === undefined);
			// Adjacency proves nothing is missing only once the group finished cleanly: a truncated
			// or still-open group may have lost its tail.
			const ended = this.#ended(first);
			const truncated = ended instanceof Error;
			const adjacent = next?.consumer.sequence === first.consumer.sequence + 1n;
			const continued = (ended === null && adjacent) || ptsContiguous(first.end ?? this.#presentedEnd, nextStart);
			if (marker || discarded || truncated || !continued) this.#lost = true;
			skipped++;
			if (!this.#discard(first)) break;
		}

		if (skipped) {
			// What the start would wait for is older than the reach, so it is past. The reader still
			// warms until the start's target, as it would had the wait ended on its own.
			if (this.#starting) {
				this.#starting = false;
				this.#target = this.#startTarget(this.#reach(this.#delay.peek()));
			}
			console.warn(
				`skipping slow groups: track=${this.#track.name} ${start} -> ${this.#active} count=${skipped}`,
			);
			this.#updateBuffered();
		}
		return skipped > 0;
	}

	// The earliest and latest known frames of the buffered groups from each index on: the span left
	// once that many of the oldest groups are skipped, undefined where none of them has a frame.
	#spans(): Spans {
		const count = this.#groups.length;
		const spans: Spans = { min: new Array(count), max: new Array(count) };
		let min: number | undefined;
		let max: number | undefined;
		for (let i = count - 1; i >= 0; i--) {
			const group = this.#groups[i];
			if (group.latest !== undefined) {
				const frame = group.frames.at(0)?.timestamp.asMicros() ?? group.latest;
				if (min === undefined || frame < min) min = frame;
				if (max === undefined || group.latest > max) max = group.latest;
			}
			spans.min[i] = min;
			spans.max[i] = max;
		}
		return spans;
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
		const nextStart = next?.frames.at(0)?.timestamp.asMicros();
		if (!next || nextStart === undefined || active.end < nextStart) return false;

		this.#groups.shift();
		console.warn(`skipping covered group: ${active.consumer.sequence} -> ${next.consumer.sequence}`);
		this.#pass(active, next.consumer.sequence);

		this.#discard(active);
		this.#updateBuffered();
		return true;
	}

	// Move on from the stalled head once the max buffer is full, since it can hold no more of what it
	// waits behind, to the next group even with nothing of it read yet: that one reads as the head, and
	// each such skip refunds what the group left behind held. Only fires when the head is fully consumed
	// and still open, so we
	// never drop frames the consumer hasn't seen: #look already read what the transport holds for it.
	// Returns true if it skipped a group.
	#tryPressureSkip(): boolean {
		if (this.#active === undefined || !this.#full()) return false;

		const head = this.#groups[0];
		if (!head || head.consumer.sequence !== this.#active) return false;
		if (head.fault || head.frames.length > 0 || this.#ended(head) !== undefined) return false;

		const next = this.#groups[1];
		if (!next) return false;

		this.#groups.shift();
		console.warn(
			`skipping a stalled group past the max buffer: track=${this.#track.name} ${head.consumer.sequence} -> ${next.consumer.sequence}`,
		);
		// Still open, so its tail is lost unless the next group picks up where it ended, which nothing
		// proves before its start is read. A marker group stops the timeline, so moving past one loses it.
		const nextStart = next.frames.at(0)?.timestamp.asMicros();
		const marker = !head.empty && !head.media;
		if (marker || !ptsContiguous(head.end ?? this.#presentedEnd, nextStart)) this.#lost = true;
		this.#pass(head, next.consumer.sequence);

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

			// The start may wait for a group covering the reach, the min delay plus the warmup.
			const starting = this.#reachStart(ended !== undefined);

			// If #active points below all buffered groups -- e.g. a finished group's `+ 1`
			// fallback fired because no later group was buffered yet, and the real (large-gap,
			// non-sequential) next group has since arrived -- promote #active to the first real
			// group so delivery resumes instead of stalling on a nonexistent sequence.
			// Before any media is delivered there is no timeline hole to wait for, so this exception
			// only applies at startup.
			// Otherwise promote #active when the first buffered group continues the timeline we left off at,
			// when a completed empty group can be walked (empty groups mean nothing), or when the hole
			// is proven: everything a missing group could still present ends where the first group past
			// it begins, so once that start falls the max delay behind the newest content, all of it
			// would arrive too late to play. Mirrors Rust's `aged`. Where presentation left off proves
			// only the oldest of it stale, and groups go out newest first, so a flush's newest group lands
			// just ahead of the groups behind it, which would then arrive below the cursor and never play.
			// Proving it here matters: #checkMaxDelay would instead drop the head, the very group to play
			// next.
			// After track termination no missing group can arrive, so drain across any remaining gap.
			// A missing sequence also expires on elapsed monotonic time, so timestamp-free markers
			// behind a hole can end a paused track without more arrivals. It is late since a group
			// past it arrived, not since the cursor got here: once the edge stops moving, a backlog
			// with every other group missing would otherwise drain one budget per hole.
			if (starting === undefined && this.#active !== undefined && this.#groups.length > 0) {
				const head = this.#groups[0];
				if (head.consumer.sequence > this.#active) {
					this.#hole = head.late;
					// Joining at the first arrival leaves no hole to wait for; a floor does.
					const startup = this.#start === undefined && !this.#floored;
					const contiguous = ptsContiguous(this.#presentedEnd, head.frames.at(0)?.timestamp.asMicros());
					const empty = head.empty && head.consumer.done;
					const wait = this.#holeWait(this.#delay.peek(), this.#wait.peek());
					const bound = this.#groups.find((group) => group.start !== undefined)?.start;
					const edge = this.#liveEdge();
					// Measured against the max delay, not the wait: a reader starved of CPU takes in a burst of
					// groups at once, and the missing one may land in the same burst.
					const aged =
						bound !== undefined && edge !== undefined && edge - bound >= maxMicros(this.#delay.peek());
					const elapsed = now().since(this.#hole).asMicros() >= wait;
					// The ranges ask for nothing in between, so nothing there is owed.
					const next = this.#track.nextInRange(this.#active);
					const excluded = next === undefined || next >= head.consumer.sequence;
					// The groups past the hole already fill the max buffer, so it can hold none of what would arrive into it.
					const full = this.#full();
					if (startup || empty || contiguous || aged || elapsed || excluded || full || ended !== undefined) {
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
			if (
				starting === undefined &&
				head &&
				this.#active !== undefined &&
				head.consumer.sequence <= this.#active
			) {
				const seq = head.consumer.sequence;
				const frame = head.frames.shift();
				if (frame) {
					this.#refund(head, head.costs.shift() ?? 0);
					const end = this.#format.end?.(frame);
					if (end !== undefined) {
						this.#updateBuffered();
						// A trailing endpoint is the last frame's duration: it moves the edge, nothing else.
						if (seq === this.#mediaGroup) {
							this.#advance(end.asMicros());
							continue;
						}
						return this.#emit({ kind: "end", group: seq, end });
					}
					this.#mediaGroup = seq;

					// A group covering the target began before it, so the reader warms until the target too.
					const at = frame.timestamp.asMicros();
					this.#warmed ??= Math.max(at, this.#target ?? at) + this.#warmup.asMicros();
					if (this.#start === undefined || seq > this.#start.group) {
						this.#floor = this.#start?.timestamp;
						this.#start = { group: seq, timestamp: at };
					}
					if (this.#floor !== undefined && at < this.#floor) {
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

				// Remove the group once the transport has settled it and every frame is read: one still
				// downloading may momentarily drain its buffer, and removing it then silently truncates
				// its tail. Its watcher wakes this read whenever the group gains a frame or ends, and
				// #checkMaxDelay bounds how long a stalled head can hold delivery up.
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
						const next = this.#groups.at(0);
						this.#pass(head, continues(head, next) ? next.consumer.sequence : seq + 1n);
					}
					head.consumer.close();
					this.#release(head);
					const truncated = settled instanceof Error;
					if (truncated) this.#lost = true;
					this.#updateBuffered();
					// A data track's marker carries no timestamp, so it ends where this reader got to.
					// Before the first item there is nothing to end.
					if (head.bare && !truncated && this.#edge !== undefined) {
						return this.#emit({ kind: "end", group: seq, end: Duration.micros(this.#edge) });
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
			const since = starting ?? this.#hole;
			if (since !== undefined) {
				timer = new Effect();
				timer.run((effect) => {
					const delay = effect.get(this.#delay);
					// The start waits out its reach, and a hole the wait, which an unlimited one never does.
					const budget =
						starting === undefined ? this.#holeWait(delay, effect.get(this.#wait)) : this.#reach(delay);
					if (!Number.isFinite(budget)) return;
					effect.timer(() => this.#notify?.(), Duration.micros(budget).saturatingSub(now().since(since)));
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

	// How long a missing group is waited for, in microseconds: the wait, at most the max delay.
	#holeWait(delay: Moq.Track.Delay, wait: Duration | undefined): number {
		const max = maxMicros(delay);
		return wait === undefined ? max : Math.min(wait.asMicros(), max);
	}

	// How far behind the live edge the start reaches, in microseconds: the min delay plus the warmup.
	// The publisher serves nothing older than the max delay, so a min delay past it reaches no further.
	#reach(delay: Moq.Track.Delay): number {
		return Math.min(delay.min?.asMicros() ?? 0, maxMicros(delay)) + this.#warmup.asMicros();
	}

	// While the start still waits for a group covering the live edge less the min delay and the
	// warmup, when the newest group had landed as the first one arrived: the wait ends a reach after
	// it. Undefined once the start is free to go, for good.
	#reachStart(ended: boolean): Instant | undefined {
		if (!this.#starting) return undefined;
		// Nothing has a wire frame yet, so nothing can start either.
		if (!this.#groups.some((group) => !group.empty)) return undefined;

		const reach = this.#reach(this.#delay.peek());
		// The newest group covers a zero reach.
		if (reach === 0) {
			this.#started();
			return undefined;
		}
		this.#anchor ??= this.#groups[this.#groups.length - 1].consumer.arrived;
		const target = this.#startTarget(reach);
		const covered =
			target !== undefined && this.#groups.some((group) => group.start !== undefined && group.start <= target);
		// Nothing earlier can arrive or be held.
		const lowest = this.#groups[0].consumer.sequence === 0n;
		// Wall time: the newest group landed a whole reach ago.
		const elapsed = now().since(this.#anchor).asMicros() >= reach;
		if (ended || this.#full() || target === undefined || covered || lowest || elapsed) {
			this.#started();
			this.#target = target;
			return undefined;
		}
		return this.#anchor;
	}

	// The start's wait ended: delivery starts at the lowest group that arrived, as Rust's.
	#started(): void {
		this.#starting = false;
		this.#active = this.#groups[0]?.consumer.sequence ?? this.#active;
	}

	// The point the start reaches back to: the live edge (the newest content) less `reach`. Undefined
	// for a zero reach, which the newest group covers, and before any timed content, since untimed
	// content such as a marker has no edge to reach back from.
	// A caller that already knows the live edge passes it, sparing a scan of every group.
	#startTarget(reach: number, known?: number): number | undefined {
		if (reach === 0) return undefined;
		const edge = known ?? this.#liveEdge();
		// Media starts at zero, so a reach past it is covered there.
		return edge === undefined ? undefined : Math.max(0, edge - reach);
	}

	// The newest known frame timestamp across the buffered groups.
	#liveEdge(): number | undefined {
		let edge: number | undefined;
		for (const group of this.#groups) {
			if (group.latest !== undefined && (edge === undefined || group.latest > edge)) edge = group.latest;
		}
		return edge;
	}

	// How stale the buffered span may get before the age skip drops its oldest group, in microseconds:
	// the max delay, plus the warmup while the reader warms.
	#ageBudget(): number {
		const maxDelay = maxMicros(this.#delay.peek());
		return this.#warming() ? maxDelay + this.#warmup.asMicros() : maxDelay;
	}

	// The reader has not yet read past the start's target and the warmup past its first frame.
	#warming(): boolean {
		return this.#warmed === undefined || (this.#edge ?? 0) < this.#warmed;
	}

	// Return `read`, or a gap ahead of it when media went missing after an earlier item.
	#emit(read: Exclude<Read, { kind: "gap" }>): Read {
		const resume = (read.kind === "frame" ? read.frame.timestamp : read.end).asMicros();
		const edge = this.#edge;
		const lost = this.#lost;
		this.#lost = false;
		this.#advance(read.kind === "frame" ? resume + (read.frame.duration?.asMicros() ?? 0) : resume);
		if (!lost || edge === undefined) return read;

		// A next group may start before the edge (an overlapping endpoint), so the span bottoms out at zero.
		const timestamp = Math.min(edge, resume);
		this.#next = read;
		return { kind: "gap", timestamp: Duration.micros(timestamp), duration: Duration.micros(resume - timestamp) };
	}

	#advance(end: number): void {
		if (this.#edge === undefined || end > this.#edge) this.#edge = end;
	}

	#updateBuffered(): void {
		const ranges: BufferedRanges = [];

		let prev: Group | undefined;
		// The open range, its end in microseconds: only the merged ranges become Durations, not
		// every group on every update.
		let start: Duration | undefined;
		let end = 0;

		for (const group of this.#groups) {
			const first = group.frames.at(0);
			if (!first || group.latest === undefined) continue;

			const contiguous = prev?.consumer.done && prev.consumer.sequence + 1n === group.consumer.sequence;
			if (start !== undefined && (end >= first.timestamp.asMicros() || contiguous)) {
				end = Math.max(end, group.latest);
			} else {
				if (start !== undefined) ranges.push({ start, end: Duration.micros(end) });
				start = first.timestamp;
				end = group.latest;
			}

			prev = group;
		}
		if (start !== undefined) ranges.push({ start, end: Duration.micros(end) });

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
