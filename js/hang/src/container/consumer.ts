import { Duration, Instant, now } from "@moq/clock";
import type * as Moq from "@moq/net";
import { Effect, type Getter, type GetterInit, getter, Once, race, Signal } from "@moq/signals";
import type { Format } from "./format.ts";
import { Jitter } from "./jitter.ts";
import type { BufferedRanges, Frame, ReceivedFrame } from "./types.ts";

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
	 * `min` is how far behind the live edge the subscription starts, plus any
	 * {@link ConsumerProps.warmup}: the publisher resolves a live join that far back, and the reader
	 * starts at the first group its ordered read hands out. An unstated (`null`) `min` reads as zero.
	 * `max` alone gives up a missing group: the ordered read waits for one until where the group past
	 * it begins falls `max` behind the newest content. `max` is also measured as the span from the
	 * oldest buffered frame to the newest, so it bounds how long a late group that did arrive is
	 * waited for. It is the local half of the subscription's `delay.max`; both measure the same
	 * budget, one on the wire and one as frames are read. It bounds a join's lateness too: a backlog
	 * that landed before the first read starts at the first group whose first frame is within it,
	 * plus any warmup, of the newest content, though the start keeps the group covering the live edge
	 * less `min` and the warmup.
	 */
	// Read-only: a Getter (e.g. another component's output) is accepted directly.
	delay?: GetterInit<Moq.Track.Delay>;
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
	 * Opus decoder. Pass the catalog audio rendition's `warmup`. Mirrors Rust's `container::Playout`.
	 *
	 * Put the min and max delay plus the warmup on the subscription, since the publisher starts a
	 * live join at the min and skips what falls outside the max, so the first group the ordered read
	 * hands out covers the live edge less `delay.min` and the warmup. Until the reader is the warmup
	 * past its first frame, the age skip keeps a group that covers the reach and tolerates the
	 * warmup, so the media leading up to the start is not dropped as stale. After that, the max delay
	 * alone governs it.
	 */
	warmup?: Duration;
	/**
	 * The consumer this one replaces on the same path, such as the previous rendition.
	 *
	 * Its arrival estimate carries over instead of restarting from the cold-start prior, so a
	 * switch neither lowers nor raises {@link Consumer.spread} on its own.
	 */
	predecessor?: Consumer;
	/**
	 * Read the media from this ordered reader of the same track rather than from `track` itself. It
	 * must be a second subscription, never `track`'s own, which then keeps reading in arrival order
	 * to feed the estimate: every frame as it lands, late media the ordered read gives up included.
	 * Without it the consumer reads `track.ordered()`, and the estimate learns from the frames that
	 * read hands out and from when each group it gives up as a join's backlog landed. Either way the
	 * ordered read decides every hole: a missing group is a hole once it reports a gap and the media
	 * after it does not continue the timeline, and never waited for here. Its `delay.max` is how long
	 * a hole is waited for, so size it to what the player can still play late.
	 */
	ordered?: Moq.Track.Ordered;
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
	frames: ReceivedFrame[]; // decode order
	gap?: boolean; // The ordered read gave up on groups just below this one.
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

// The deepest reordering a codec allows, in pictures: H.264 and HEVC hold at most 16 frames, so 32
// fields. A reader measures a track's reordering and its last picture's duration within this many of a
// group's latest presentation times. Mirrors Rust's `REORDER_MAX`.
const REORDER_MAX = 32;

// How the groups read so far run, for judging a step over keys nothing declared skipped. Mirrors Rust's
// `Cadence`.
interface Cadence {
	// One picture, in microseconds, for a group whose frames carry no duration: its highest timestamp is
	// where its last picture starts, not ends. The largest step between its latest presentation times,
	// so a field/frame mix allows a frame. Zero once a group's end was exact.
	slack: number;
	// The most frames that decoded ahead of a frame they present after, in any group read: an open GOP's
	// leading pictures decode after the keyframe, so its lowest picture comes within that many.
	depth: number;
}

// The max delay in microseconds, infinite when unlimited.
function maxMicros(delay: Moq.Track.Delay): number {
	return delay.max === null ? Number.POSITIVE_INFINITY : (delay.max?.asMicros() ?? 0);
}

/**
 * Reads frames from a MoQ track in group order through its ordered read, buffering groups and skipping
 * ones that fall the max delay behind. The ordered read decides every hole and where delivery starts:
 * at the first group it hands out.
 */
export class Consumer {
	#track: Moq.Track.Subscriber;
	// The read that decides every hole and delivers the media.
	#ordered: Moq.Track.Ordered;
	// `#track` is a second subscription read in arrival order, which alone feeds the estimate.
	#observing: boolean;
	// The ordered read gave up on groups since the last one it returned.
	#gap = false;
	// When each gap given up before the join's boundary was known landed, by the key it ends at: told to
	// the estimate once the boundary is known. Only gaps ahead of the first group are kept, which bounds it.
	#unjoined?: { end: bigint; landed: Instant }[] = [];
	#format: Format;
	#delay: Getter<Moq.Track.Delay>;
	#maxBuffer: number;
	// How much further than the min delay the start reaches, and the age skip tolerates while warming.
	#warmup: Duration;
	// Where the warming ends: the warmup past the first frame read. Undefined before the first frame.
	#warmed?: number;
	// What the buffered groups hold, against the max buffer.
	#used = 0;
	// Group watchers and the track loop parked on a full max buffer.
	#room: (() => void)[] = [];
	#groups: Group[] = [];
	#active?: bigint; // the cursor: the start of the group being read, or a key just past it
	// Presentation end (max PTS + duration) of the group we most recently advanced past, so next()'s
	// promotion guard can tell a timeline-continuous next group from one sitting after a gap.
	// Written only by #pass, which every move past a group goes through.
	#presentedEnd?: number;
	// How the groups read so far run, for judging a step whose group shows no proof of its own.
	#cadence: Cadence = { slack: 0, depth: 0 };
	// The latest presentation times of the media read from the group being read, ascending, at most
	// REORDER_MAX.
	#pictures: number[] = [];
	// Group of the last media frame next() returned, so an endpoint after it reads as that frame's
	// duration rather than a declared stop.
	#mediaGroup?: bigint;
	// Media went missing since the last item read, so the next one is preceded by a gap.
	#lost = false;
	// The ordered reader gave up on keys since the last item read: a loss only if the next item does
	// not continue the timeline, since a publisher keying groups by timestamp never produced most keys.
	#gapped?: { from: bigint; to: bigint };
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
	// The publisher's largest group when the subscription began: one at or below it was produced
	// before, so dropping it unread skips the join's backlog.
	#joined?: bigint;
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
	#mediaStart?: bigint;

	#signals = new Effect();
	#closed = new Once<Error | null>();
	// Set once the first group arrived, which establishes the subscription like its info.
	#tuned = new Once<true>();

	/** Start consuming the given track, decoding frames with `props.format`. */
	constructor(track: Moq.Track.Subscriber, props: ConsumerProps) {
		this.#track = track;
		this.#observing = props.ordered !== undefined;
		const ordered = props.ordered ?? track.ordered();
		this.#ordered = ordered;
		this.#format = props.format;
		this.#delay = getter(props.delay ?? {});
		this.#maxBuffer = props.maxBuffer ?? DEFAULT_MAX_BUFFER;
		this.#warmup = (props.warmup ?? Duration.ZERO).min(DELAY_MAX);
		this.#jitter = props.predecessor ? props.predecessor.#jitter.fork() : new Jitter();
		this.#spread = new Signal(Duration.millis(this.#jitter.measured));
		this.spread = this.#spread;

		// Where the publisher stood as the subscription began, read once its answer shows the
		// subscription established: after it rather than from a cached group that raced it, and before
		// live groups landing later raise the largest past it. Off the delivery path: a range capped
		// below the largest never reports one, which reports no backlog rather than stalling delivery.
		this.#signals.spawn(async () => {
			await race([ordered.info().catch(() => undefined), this.#tuned]);
			this.#joined = (await ordered.largest())?.group;
			this.#joinBacklog();
		});
		this.#signals.spawn(this.#run.bind(this, ordered));
		if (this.#observing) this.#signals.spawn(this.#observe.bind(this));
		this.#signals.cleanup(() => {
			if (this.#observing) this.#track.close();
			this.#ordered.close();
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

	// Read groups in key order from `ordered`, noting each gap it reports on the group after it.
	async #run(ordered: Moq.Track.Ordered) {
		try {
			for (;;) {
				// Past the max buffer, later groups wait in the transport's cache.
				while (this.#full() && this.#groups.length >= 2 && !this.#signals.abort.aborted) await this.#park();
				if (this.#signals.abort.aborted) break;

				const item = await ordered.nextItem();
				if (!item) break;
				// Closed while the read waited: cleanup already ran, so nothing would release it.
				if (this.#signals.abort.aborted) {
					item.group?.close();
					break;
				}
				if (this.#tuned.peek() === undefined) this.#tuned.set(true);
				if (item.kind === "gap") {
					this.#gap = true;
					if (item.landed) this.#backlog(item.end, item.landed);
					continue;
				}
				// Delivery starts at the first group: the reader already skipped what is too stale to
				// play, and waited for the groups below it that its feed owed.
				if (this.#active === undefined) this.#active = item.group.start;
				this.#add(item.group, this.#gap);
				this.#gap = false;
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

	// Tell the estimate when the groups the ordered read gave up below `end` landed, if the publisher's
	// largest as the subscription began reached them all: they were produced before it, a join's
	// backlog, so the estimate learns when the backlog finished arriving though nothing of it was read.
	// One produced since, such as an outage's, is no backlog, and a gap reaching past the join may
	// land as one, so it folds nothing. The estimate observes every arrival itself when `#track` feeds it.
	#backlog(end: bigint, landed: Instant): void {
		if (this.#observing) return;
		if (this.#joined === undefined) {
			// Only gaps ahead of the first group wait for it, which bounds the list a peer can grow.
			if (this.#active === undefined) this.#unjoined?.push({ end, landed });
			return;
		}
		if (end - 1n <= this.#joined) this.#jitter.backlog(landed.since(Instant.ZERO).asMillis());
	}

	// The join's boundary is known: judge the gaps given up before it was.
	#joinBacklog(): void {
		const unjoined = this.#unjoined ?? [];
		this.#unjoined = undefined;
		for (const { end, landed } of unjoined) this.#backlog(end, landed);
	}

	// Buffer a group in start order and watch it for frames.
	#add(consumer: Moq.Group.Consumer, gap = false): void {
		const group: Group = {
			consumer,
			frames: [],
			costs: [],
			gap,
			empty: true,
			media: false,
			index: 0,
			charged: 0,
		};
		this.#charge(group, GROUP_COST);

		// The ordered read hands groups out in key order, so each goes last.
		this.#groups.push(group);
		// Group overhead alone can fill the max buffer, past which next() moves on from a stalled head.
		if (this.#full()) {
			this.#notify?.();
			this.#notify = undefined;
		}

		this.#signals.spawn(this.#watch.bind(this, group));
	}

	// Feed the arrival estimate from every frame of `#track` as it lands, while the ordered read
	// delivers: late media that read gives up on must still move the estimate, or it could never rise.
	async #observe() {
		try {
			for (;;) {
				const group = await this.#track.recvGroup();
				if (!group) return;
				// Closed while the read waited: cleanup already ran, so nothing would release it.
				if (this.#signals.abort.aborted) {
					group.close();
					return;
				}
				this.#signals.spawn(this.#observeGroup.bind(this, group));
			}
		} catch {
			// The ordered reader reports how the track ended.
		}
	}

	async #observeGroup(group: Moq.Group.Consumer) {
		let media = false;
		try {
			for (;;) {
				const wire = await group.readFrame();
				if (!wire || this.#signals.abort.aborted) return;
				// A payload that fails to decode is the ordered read's to raise.
				let decoded: Frame[];
				try {
					decoded = this.#format.decode(wire.payload, wire.timestamp);
				} catch {
					return;
				}
				const arrived = (wire.arrived ?? now()).since(Instant.ZERO).asMillis();
				if (this.#format.kind === "data" && decoded.length === 0) this.#declare(group.start);
				for (const sample of decoded) {
					const marker = this.#format.end?.(sample) !== undefined;
					this.#note(group.start, marker, media, sample.timestamp, arrived);
					if (!marker) media = true;
				}
			}
		} catch {
			// A group cut short has no more frames to observe.
		} finally {
			group.close();
		}
	}

	// Observe one decoded sample of the group keyed `start` for the arrival estimate: media is observed,
	// and a marker ahead of any media (`media` false) declares an end.
	#note(start: bigint, marker: boolean, media: boolean, timestamp: Duration, arrived: number): void {
		// Observed before any gap handling or age budget: a target derived from what survives the
		// budget would only ever confirm the budget it was cut to.
		if (!marker) {
			if (this.#mediaStart === undefined || start > this.#mediaStart) this.#mediaStart = start;
			if (this.#declared !== undefined && start > this.#declared) {
				this.#jitter.end();
				this.#declared = undefined;
			}
			this.#jitter.observe(arrived, timestamp.asMillis());
			const spread = this.#jitter.measured;
			if (spread !== this.#spread.peek().asMillis()) this.#spread.set(Duration.millis(spread));
		} else if (!media) {
			// Only a marker ahead of any media declares an end; one after it is a frame's endpoint.
			this.#declare(start);
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
		// A closed consumer abandoned whatever was still arriving.
		if (!this.#delivered || this.#finished.peek() || this.#signals.abort.aborted) return;
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
					if (group.consumer.start !== this.#active) ahead = true;
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

	// Note an end the publisher declared in the group keyed `start`, which a later group's first frame resumes from.
	#declare(start: bigint): void {
		if (this.#mediaStart !== undefined && start < this.#mediaStart) return;
		if (this.#declared === undefined || start > this.#declared) this.#declared = start;
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
			if (group.bare && !this.#observing) this.#declare(group.consumer.start);
		}

		// Decoded payloads are views of the wire payload, which stays allocated until the last of them is
		// read, so that one also holds whatever of it the views leave out, such as a large header.
		let views = 0;
		for (const sample of decoded) {
			const marker = this.#format.end?.(sample) !== undefined;
			// Observed here, as it comes off the transport, unless #observe sees every arrival instead.
			if (!this.#observing) this.#note(group.consumer.start, marker, group.media, sample.timestamp, arrived);
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

	// Note a media frame read from the group being read, presenting at `at`: how many frames read before
	// it present after it is how far the track reorders.
	#read(at: number): void {
		const latest = this.#pictures;
		let below = latest.length;
		while (below > 0 && latest[below - 1] > at) below--;
		this.#cadence.depth = Math.max(this.#cadence.depth, latest.length - below);
		if (latest.length < REORDER_MAX) {
			latest.splice(below, 0, at);
		} else if (below > 0) {
			// Full: the earliest leaves, unless this frame presents before every time kept.
			latest.shift();
			latest.splice(below - 1, 0, at);
		}
	}

	// A group read whole ended at `end`: exactly when a duration or its end marker reaches past its
	// frames, otherwise one picture past the highest.
	#reachedEnd(end: number): void {
		const latest = this.#pictures;
		const highest = latest.at(-1);
		if (highest === undefined) return;
		if (end > highest) {
			this.#cadence.slack = 0;
			return;
		}
		let step = 0;
		for (let i = 1; i < latest.length; i++) step = Math.max(step, latest[i] - latest[i - 1]);
		if (step > 0) this.#cadence.slack = step;
	}

	// Whether `head`, the next group past keys nothing declared skipped, continues where the content
	// read so far ended: one of its frames presents by then, within one picture. Its lowest picture
	// decodes within the track's reorder depth of its first, so it looks no further, nor past its end or
	// a full max buffer. Undefined until it shows. Before the first item nothing was read to lose, so its
	// first frame decides.
	#continues(head: Group): boolean | undefined {
		const end = this.#edge === undefined ? undefined : this.#presentedEnd;
		const bound = end === undefined ? undefined : end + this.#cadence.slack;
		const depth = bound === undefined ? 0 : this.#cadence.depth;
		const frames = head.frames;
		for (let i = 0; i < frames.length && i <= depth; i++) {
			if (ptsContiguous(bound, frames[i].timestamp.asMicros())) return true;
		}
		if (frames.length > depth || head.consumer.done || head.fault !== undefined) return false;
		// Only reading frames frees a full max buffer, so a head holding one judges by what it holds.
		if (frames.length > 0 && this.#full()) return false;
		return undefined;
	}

	// Skip the oldest groups while the buffered span exceeds the max delay. Returns true if any was skipped.
	#checkMaxDelay(): boolean {
		if (this.#active === undefined) return false;

		let skipped = 0;
		const start = this.#groups[0]?.consumer.start;
		// The span left after each further skip, computed once the first skip shows more may follow.
		let spans: Spans | undefined;
		// The index of the first stamped group past the head, kept across skips so a run of unstamped
		// groups ahead of it stays linear: each skip shifts it down one, and only a skip onto it rescans.
		let stamped = 0;

		// Keep skipping the oldest group while the buffered span exceeds the max delay.
		// This also handles holes between group starts: if #active points to a missing
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
			this.#pass(first, this.#groups[0]?.consumer.start);

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
			// The publisher declares the starts it never produced, so a run it skipped holds no group.
			const declared =
				next !== undefined && this.#ordered.isSkipped(first.consumer.start + 1n, next.consumer.start);
			const continued = (ended === null && declared) || ptsContiguous(first.end ?? this.#presentedEnd, nextStart);
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
		if (!active || active.consumer.start !== this.#active) return false;
		if (this.#ended(active) !== undefined || active.frames.length > 0 || active.end === undefined) return false;

		const next = this.#groups[1];
		const nextStart = next?.frames.at(0)?.timestamp.asMicros();
		if (!next || nextStart === undefined || active.end < nextStart) return false;

		// Routine for a group of one frame whose stream finishes after the next one starts, and nothing
		// is lost, so it says nothing.
		this.#groups.shift();
		this.#pass(active, next.consumer.start);

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
		if (!head || head.consumer.start !== this.#active) return false;
		if (head.fault || head.frames.length > 0 || this.#ended(head) !== undefined) return false;

		const next = this.#groups[1];
		if (!next) return false;

		this.#groups.shift();
		console.warn(
			`skipping a stalled group past the max buffer: track=${this.#track.name} ${head.consumer.start} -> ${next.consumer.start}`,
		);
		// Still open, so its tail is lost unless the next group picks up where it ended, which nothing
		// proves before its start is read. A marker group stops the timeline, so moving past one loses it.
		const nextStart = next.frames.at(0)?.timestamp.asMicros();
		const marker = !head.empty && !head.media;
		if (marker || !ptsContiguous(head.end ?? this.#presentedEnd, nextStart)) this.#lost = true;
		this.#pass(head, next.consumer.start);

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
			group.consumer.start > start.group &&
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
		if (this.#observing) this.#track.close(error);
		this.#ordered.close(error);
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
			if (this.#groups.length === 0) {
				if (ended !== undefined) {
					if (ended instanceof Error) throw ended;
					return undefined;
				}
			}

			// The ordered read hands a group out only once it resolved every key below it, so the cursor
			// steps onto the next buffered group once it can tell whether the step lost media. Nothing is
			// lost where the publisher declared the keys between skipped, where the group continues the
			// timeline within 1 ms, or onto an empty group. Anything else is a loss, whether the ordered
			// read gave the keys up as a gap or stepped over them as a local track does, which owes
			// nothing and reports none: without a gap, the step waits for the group's first frame or its
			// end to show where it starts. A finished group leaves the cursor just past its start, below
			// the next buffered group, which this step then reaches.
			if (this.#active !== undefined && this.#groups.length > 0) {
				const head = this.#groups[0];
				if (head.consumer.start > this.#active) {
					// An open GOP's leading pictures decode after its keyframe but present before it, so
					// frames after the first may show the head continues; until then its first frame waits.
					const verdict = this.#ordered.isSkipped(this.#active, head.consumer.start) || this.#continues(head);
					const contiguous = verdict === true;
					const empty = head.empty && head.consumer.done;
					// A payload that failed to decode gives no timestamp, so its fault settles the step,
					// for the read below to raise. The read's own end says nothing of where an open head
					// starts, and reading waits on the head either way. Keys the ordered read gave up on
					// wait for the verdict too, as Rust's do: a group keyed by its timestamp may continue
					// the timeline across keys nobody produced.
					const known = verdict !== undefined || head.fault !== undefined || head.consumer.done;
					if (known) {
						// A gap an empty group carries still marks the next frame, which follows the loss.
						if (head.gap && !contiguous) {
							// The head's first frame may not have landed yet, so the next item read decides.
							this.#gapped = { from: this.#gapped?.from ?? this.#active, to: head.consumer.start };
						} else if (!contiguous && !empty) {
							this.#lost = true;
							// Before the first media, nothing was read to lose.
							if (this.#start !== undefined)
								console.warn(
									`skipping missing groups: track=${this.#track.name} ${this.#active} -> ${head.consumer.start}`,
								);
						}
						this.#active = head.consumer.start;
					}
				}
			}

			const head = this.#groups.at(0);
			if (head && this.#active !== undefined && head.consumer.start <= this.#active) {
				const key = head.consumer.start;
				const frame = head.frames.shift();
				if (frame) {
					this.#refund(head, head.costs.shift() ?? 0);
					const end = this.#format.end?.(frame);
					if (end !== undefined) {
						this.#updateBuffered();
						// A trailing endpoint is the last frame's duration: it moves the edge, nothing else.
						if (key === this.#mediaGroup) {
							this.#advance(end.asMicros());
							continue;
						}
						return this.#emit({ kind: "end", group: key, end });
					}
					this.#mediaGroup = key;

					const at = frame.timestamp.asMicros();
					this.#warmed ??= at + this.#warmup.asMicros();
					if (this.#start === undefined || key > this.#start.group) {
						this.#floor = this.#start?.timestamp;
						this.#start = { group: key, timestamp: at };
						this.#pictures = [];
					}
					this.#read(at);
					if (this.#floor !== undefined && at < this.#floor) {
						this.#abort(new Error("frame timestamp is below the previous group start"));
						throw this.#error;
					}
					this.#updateBuffered();
					return this.#emit({ kind: "frame", group: key, frame });
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
					// Step just past this group's start. Starts are timestamps, so the next group rarely
					// sits there: the promotion guard above steps to it once the publisher declared the run
					// between skipped or its first frame continues this group's timeline, as Rust's
					// `poll_continues` does, and otherwise waits for the hole like any other.
					if (key === this.#active) {
						this.#pass(head, key + 1n);
						if (settled === null && head.end !== undefined && this.#start?.group === key)
							this.#reachedEnd(head.end);
					}
					head.consumer.close();
					this.#release(head);
					const truncated = settled instanceof Error;
					if (truncated) this.#lost = true;
					this.#updateBuffered();
					// A data track's marker carries no timestamp, so it ends where this reader got to.
					// Before the first item there is nothing to end.
					if (head.bare && !truncated && this.#edge !== undefined) {
						return this.#emit({ kind: "end", group: key, end: Duration.micros(this.#edge) }, false);
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

	// How far behind the live edge the start reaches, in microseconds: the min delay plus the warmup.
	// The publisher serves nothing older than the max delay, so a min delay past it reaches no further.
	#reach(delay: Moq.Track.Delay): number {
		return Math.min(delay.min?.asMicros() ?? 0, maxMicros(delay)) + this.#warmup.asMicros();
	}

	// The point the start reaches back to: the live edge (the newest content) less `reach`. Undefined
	// for a zero reach, which the newest group covers.
	#startTarget(reach: number, edge: number): number | undefined {
		if (reach === 0) return undefined;
		// Media starts at zero, so a reach past it is covered there.
		return Math.max(0, edge - reach);
	}

	// How stale the buffered span may get before the age skip drops its oldest group, in microseconds:
	// the max delay, plus the warmup while the reader warms.
	#ageBudget(): number {
		const maxDelay = maxMicros(this.#delay.peek());
		return this.#warming() ? maxDelay + this.#warmup.asMicros() : maxDelay;
	}

	// The reader has not yet read the warmup past its first frame.
	#warming(): boolean {
		return this.#warmed === undefined || (this.#edge ?? 0) < this.#warmed;
	}

	// Return `read`, or a gap ahead of it when media went missing after an earlier item. An untimed
	// read, a data marker ending where this reader got to, cannot show the timeline continues past a gap.
	#emit(read: Exclude<Read, { kind: "gap" }>, timed = true): Read {
		const resume = (read.kind === "frame" ? read.frame.timestamp : read.end).asMicros();
		const edge = this.#edge;
		const gapped = this.#gapped;
		this.#gapped = undefined;
		let lost = this.#lost;
		this.#lost = false;
		if (gapped && edge !== undefined && (!timed || !ptsContiguous(edge, resume))) {
			lost = true;
			console.warn(`skipping missing groups: track=${this.#track.name} ${gapped.from} -> ${gapped.to}`);
		}
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

			// Nothing lies between a finished group and the next when the publisher declared the starts
			// between skipped, or when the next starts where the previous one's media ends.
			const contiguous =
				prev?.consumer.done &&
				(this.#ordered.isSkipped(prev.consumer.start + 1n, group.consumer.start) ||
					ptsContiguous(prev.end, first.timestamp.asMicros()));
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
