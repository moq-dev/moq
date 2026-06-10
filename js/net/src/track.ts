import { Signal } from "@moq/signals";
import { Group } from "./group.ts";

/** Default {@link TrackInfo.cache} window (milliseconds) when the publisher doesn't set one. */
export const DEFAULT_CACHE_MS = 5000;

/**
 * A track's immutable publisher properties, fixed for the lifetime of the track.
 *
 * A producer declares these once (via `TrackRequest.accept` or
 * {@link TrackProducer.accept}); a consumer awaits them via {@link TrackSubscriber.info}
 * (resolved from the wire TRACK_INFO on lite-05+). They map 1:1 onto TRACK_INFO.
 */
export interface TrackInfo {
	/** Hint that frames are worth compressing (e.g. a JSON catalog). */
	compress: boolean;
	/** How long (milliseconds) old groups stay available before eviction. */
	cache: number;
	/** Tie-break priority between subscriptions of equal subscriber priority. */
	priority: number;
	/** Group ordering preference (newest-first when `false`). */
	ordered: boolean;
}

/** Fill in any unset {@link TrackInfo} fields with their defaults. */
export function trackInfoDefaults(info: Partial<TrackInfo> = {}): TrackInfo {
	return {
		compress: info.compress ?? false,
		cache: info.cache ?? DEFAULT_CACHE_MS,
		priority: info.priority ?? 0,
		ordered: info.ordered ?? true,
	};
}

/** The shared state behind a {@link TrackProducer} / {@link TrackSubscriber} pair. */
export class TrackState {
	groups = new Signal<Group[]>([]);
	closed = new Signal<boolean | Error>(false);
	priority = new Signal<number | undefined>(undefined);
	/** Resolved once the producer commits the immutable properties. */
	info = new Signal<TrackInfo | undefined>(undefined);
}

/** Shared base for the two ends of a track: name, state, close, and info. */
abstract class TrackHandle {
	readonly name: string;
	readonly state: TrackState;
	readonly closed: Promise<Error | undefined>;

	constructor(name: string, state: TrackState) {
		this.name = name;
		this.state = state;

		this.closed = new Promise((resolve) => {
			const dispose = this.state.closed.subscribe((closed) => {
				if (!closed) return;
				resolve(closed instanceof Error ? closed : undefined);
				dispose();
			});
		});
	}

	/**
	 * Resolve this track's immutable publisher properties.
	 *
	 * On a producer this resolves once the info is committed (at accept time); on a
	 * consumer once the wire layer commits the TRACK_INFO it received (lite-05+) or
	 * defaults (older drafts), so awaiting it never yields a placeholder. Rejects if
	 * the track is closed before the properties are known (e.g. a rejected subscription).
	 */
	async info(): Promise<TrackInfo> {
		for (;;) {
			const info = this.state.info.peek();
			if (info) return info;

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) throw new Error("track closed before info was known");

			await Signal.race(this.state.info, this.state.closed);
		}
	}

	/** Close the track (optionally with an error), closing any pending groups. */
	close(abort?: Error) {
		this.state.closed.set(abort ?? true);
		for (const group of this.state.groups.peek()) {
			group.close(abort);
		}
	}
}

/**
 * The write side of a track, mirroring the Rust `TrackProducer`.
 *
 * Obtained from `TrackRequest.accept` (the wire asks the application for a track to
 * serve) or constructed directly for an in-process track. Writes groups that a
 * {@link TrackSubscriber} on the same {@link TrackState} reads.
 */
export class TrackProducer extends TrackHandle {
	#next?: number;

	constructor(name: string, state: TrackState = new TrackState()) {
		super(name, state);
	}

	/** Commit the immutable publisher properties, resolving {@link info}. Returns `this`. */
	accept(info: Partial<TrackInfo> = {}): this {
		this.state.info.set(trackInfoDefaults(info));
		return this;
	}

	/** A {@link TrackSubscriber} reading this in-process track's groups. */
	subscribe(): TrackSubscriber {
		return new TrackSubscriber(this.name, this.state);
	}

	/** Append a new group with the next sequence number. */
	appendGroup(): Group {
		if (this.state.closed.peek()) throw new Error("track is closed");

		const group = new Group(this.#next ?? 0);

		this.#next = group.sequence + 1;
		this.state.groups.mutate((groups) => {
			groups.push(group);
			groups.sort((a, b) => a.sequence - b.sequence);
		});

		return group;
	}

	/** Insert an existing group into the track. */
	writeGroup(group: Group) {
		if (this.state.closed.peek()) throw new Error("track is closed");

		// Only advance #next upward (for appendGroup auto-increment).
		if (group.sequence >= (this.#next ?? 0)) {
			this.#next = group.sequence + 1;
		}

		this.state.groups.mutate((groups) => {
			groups.push(group);
			groups.sort((a, b) => a.sequence - b.sequence);
		});
	}

	/** Append a frame as its own single-frame group. */
	writeFrame(frame: Uint8Array) {
		const group = this.appendGroup();
		group.writeFrame(frame);
		group.close();
	}

	writeString(str: string) {
		const group = this.appendGroup();
		group.writeString(str);
		group.close();
	}

	writeJson(json: unknown) {
		const group = this.appendGroup();
		group.writeJson(json);
		group.close();
	}

	writeBool(bool: boolean) {
		const group = this.appendGroup();
		group.writeBool(bool);
		group.close();
	}
}

/**
 * The read side of a live track subscription, mirroring the Rust `TrackSubscriber`.
 *
 * Obtained from `Broadcast.subscribe` / `TrackConsumer.subscribe`, or from
 * {@link TrackProducer.subscribe} for an in-process track. Reads the groups a
 * {@link TrackProducer} on the same {@link TrackState} writes.
 */
export class TrackSubscriber extends TrackHandle {
	#nextSequence = 0;

	/**
	 * Receive the next group available on this track, in arrival order.
	 *
	 * Groups may arrive out of order or with gaps due to network conditions.
	 * Use {@link nextGroup} for sequence order, skipping those that arrive too late.
	 */
	async recvGroup(): Promise<Group | undefined> {
		for (;;) {
			const groups = this.state.groups.peek();
			if (groups.length > 0) {
				return groups.shift();
			}

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			await Signal.race(this.state.groups, this.state.closed);
		}
	}

	/**
	 * Return the next group with a strictly-greater sequence number than the last returned.
	 *
	 * Late arrivals (sequence at or below the last returned) are silently skipped.
	 * Use {@link recvGroup} to see every group in arrival order instead.
	 */
	async nextGroup(): Promise<Group | undefined> {
		for (;;) {
			const group = await this.recvGroup();
			if (!group) return undefined;
			if (group.sequence < this.#nextSequence) {
				group.close();
				continue;
			}
			this.#nextSequence = group.sequence + 1;
			return group;
		}
	}

	async readFrame(): Promise<Uint8Array | undefined> {
		return (await this.readFrameSequence())?.data;
	}

	// Returns the sequence number of the group and frame, not just the data.
	async readFrameSequence(): Promise<{ group: number; frame: number; data: Uint8Array } | undefined> {
		for (;;) {
			const groups = this.state.groups.peek();

			// Discard old groups.
			while (groups.length > 1) {
				const frames = groups[0].state.frames.peek();
				const next = frames.shift();
				if (next) {
					const frame = groups[0].state.total.peek() - frames.length - 1;
					return { group: groups[0].sequence, frame, data: next };
				}

				// Skip this old group
				groups.shift()?.close();
			}

			// If there's no groups, wait for a new one.
			if (groups.length === 0) {
				const closed = this.state.closed.peek();
				if (closed instanceof Error) throw closed;
				if (closed) return undefined;

				await Signal.race(this.state.groups, this.state.closed);
				continue;
			}

			// If there's a group, wait for a frame.
			const group = groups[0];
			const frames = group.state.frames.peek();
			const next = frames.shift();
			if (next) {
				const frame = group.state.total.peek() - frames.length - 1;
				return { group: group.sequence, frame, data: next };
			}

			// If the track is closed, return undefined.
			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			// NOTE: We don't care if the latest group was closed or not.
			await Signal.race(this.state.groups, this.state.closed, group.state.frames);
		}
	}

	async readString(): Promise<string | undefined> {
		const next = await this.readFrame();
		if (!next) return undefined;
		return new TextDecoder().decode(next);
	}

	async readJson(): Promise<unknown | undefined> {
		const next = await this.readString();
		if (!next) return undefined;
		return JSON.parse(next);
	}

	async readBool(): Promise<boolean | undefined> {
		const next = await this.readFrame();
		if (!next) return undefined;
		if (next.byteLength !== 1 || !(next[0] === 0 || next[0] === 1)) throw new Error("invalid bool frame");
		return next[0] === 1;
	}

	/**
	 * Update this subscription's priority, triggering a SUBSCRIBE_UPDATE to the publisher.
	 */
	updatePriority(priority: number) {
		this.state.priority.set(priority, true);
	}
}
