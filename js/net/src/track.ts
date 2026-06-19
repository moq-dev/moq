import { Signal } from "@moq/signals";
import { Group } from "./group.ts";

/** Reactive backing state for a {@link Track}: buffered groups, a closed flag, and the subscription priority. */
export class TrackState {
	groups = new Signal<Group[]>([]);
	closed = new Signal<boolean | Error>(false);
	priority = new Signal<number | undefined>(undefined);
}

/** A live stream of groups within a broadcast, identified by name. */
export class Track {
	/** Name of this track within its broadcast. */
	readonly name: string;

	/** Reactive backing state. */
	state = new TrackState();
	#next?: number;
	#nextSequence = 0;

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	constructor(name: string) {
		this.name = name;

		this.closed = new Promise((resolve) => {
			const dispose = this.state.closed.subscribe((closed) => {
				if (!closed) return;
				resolve(closed instanceof Error ? closed : undefined);
				dispose();
			});
		});
	}

	/**
	 * Appends a new group to the track.
	 * @returns A GroupProducer for the new group
	 */
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

	/**
	 * Inserts an existing group into the track.
	 * @param group - The group to insert
	 */
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

	/**
	 * Appends a frame to the track in its own group.
	 *
	 * @param frame - The frame to append
	 */
	writeFrame(frame: Uint8Array) {
		const group = this.appendGroup();
		group.writeFrame(frame);
		group.close();
	}

	/** Appends a string to the track as its own single-frame group. */
	writeString(str: string) {
		const group = this.appendGroup();
		group.writeString(str);
		group.close();
	}

	/** Appends a JSON value to the track as its own single-frame group. */
	writeJson(json: unknown) {
		const group = this.appendGroup();
		group.writeJson(json);
		group.close();
	}

	/** Appends a boolean to the track as its own single-frame group. */
	writeBool(bool: boolean) {
		const group = this.appendGroup();
		group.writeBool(bool);
		group.close();
	}

	/**
	 * Receive the next group available on this track, in arrival order.
	 *
	 * Groups may arrive out of order or with gaps due to network conditions.
	 * Use {@link nextGroupOrdered} if you need groups in sequence order,
	 * skipping those that arrive too late.
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
	 * @deprecated Use {@link recvGroup} for arrival order, or {@link nextGroupOrdered} for sequence order.
	 */
	async nextGroup(): Promise<Group | undefined> {
		return this.recvGroup();
	}

	/**
	 * Return the next group with a strictly-greater sequence number than the last returned.
	 *
	 * Late arrivals (with a sequence number at or below the last one returned) are silently skipped.
	 *
	 * NOTE: This will be renamed to `nextGroup` in the next major version.
	 */
	async nextGroupOrdered(): Promise<Group | undefined> {
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

	/** Reads the next frame across all groups, discarding older groups. */
	async readFrame(): Promise<Uint8Array | undefined> {
		return (await this.readFrameSequence())?.data;
	}

	/** Reads the next frame along with its group and frame sequence numbers. */
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

	/** Reads the next frame and decodes it as a UTF-8 string. */
	async readString(): Promise<string | undefined> {
		const next = await this.readFrame();
		if (!next) return undefined;
		return new TextDecoder().decode(next);
	}

	/** Reads the next frame and parses it as JSON. */
	async readJson(): Promise<unknown | undefined> {
		const next = await this.readString();
		if (!next) return undefined;
		return JSON.parse(next);
	}

	/** Reads the next frame and decodes it as a one-byte boolean, throwing on a malformed frame. */
	async readBool(): Promise<boolean | undefined> {
		const next = await this.readFrame();
		if (!next) return undefined;
		if (next.byteLength !== 1 || !(next[0] === 0 || next[0] === 1)) throw new Error("invalid bool frame");
		return next[0] === 1;
	}

	/**
	 * Update the subscription priority. Triggers a SUBSCRIBE_UPDATE
	 * to the publisher when used on a subscribed track.
	 */
	updatePriority(priority: number) {
		this.state.priority.set(priority, true);
	}

	/**
	 * Closes the publisher and all associated groups.
	 */
	close(abort?: Error) {
		this.state.closed.set(abort ?? true);

		for (const group of this.state.groups.peek()) {
			group.close(abort);
		}
	}
}
