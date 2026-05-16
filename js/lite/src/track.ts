import { Signal } from "@moq/signals";
import { Datagram, MAX_DATAGRAM_AGE_MS, MAX_DATAGRAM_PAYLOAD } from "./datagram.ts";
import { Group } from "./group.ts";

interface DatagramSlot {
	datagram: Datagram;
	createdAt: number; // performance.now() at write time
}

export class TrackState {
	groups = new Signal<Group[]>([]);
	closed = new Signal<boolean | Error>(false);
	/** Datagrams cache; entries older than `MAX_DATAGRAM_AGE_MS` are evicted on each touch. */
	datagrams = new Signal<DatagramSlot[]>([]);
}

export class Track {
	readonly name: string;

	state = new TrackState();
	#next?: number;
	#nextSequence = 0;
	#nextDatagramSequence = 0;

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

	/**
	 * Write a datagram with an explicit sequence number.
	 *
	 * Throws if the payload exceeds {@link MAX_DATAGRAM_PAYLOAD} or the track is closed.
	 */
	writeDatagram(datagram: Datagram) {
		if (this.state.closed.peek()) throw new Error("track is closed");
		if (datagram.payload.byteLength > MAX_DATAGRAM_PAYLOAD) {
			throw new Error(`datagram payload ${datagram.payload.byteLength} > ${MAX_DATAGRAM_PAYLOAD}`);
		}
		if (datagram.sequence >= this.#nextDatagramSequence) {
			this.#nextDatagramSequence = datagram.sequence + 1;
		}
		this.#pushDatagram(datagram);
	}

	/**
	 * Append a datagram with the next auto-assigned sequence; returns the assigned value.
	 */
	appendDatagram(payload: Uint8Array): number {
		if (this.state.closed.peek()) throw new Error("track is closed");
		if (payload.byteLength > MAX_DATAGRAM_PAYLOAD) {
			throw new Error(`datagram payload ${payload.byteLength} > ${MAX_DATAGRAM_PAYLOAD}`);
		}
		const sequence = this.#nextDatagramSequence++;
		this.#pushDatagram(new Datagram(sequence, payload));
		return sequence;
	}

	#pushDatagram(datagram: Datagram) {
		const now = performance.now();
		this.state.datagrams.mutate((slots) => {
			slots.push({ datagram, createdAt: now });
			// Evict expired entries from the front. The ring stays small (33ms TTL),
			// so a linear scan is fine.
			while (slots.length > 0 && now - slots[0].createdAt > MAX_DATAGRAM_AGE_MS) {
				slots.shift();
			}
		});
	}

	/**
	 * Drop any currently-cached datagrams. Useful before the first
	 * {@link recvDatagram} for strict (`maxLatency = 0`) semantics so the
	 * consumer doesn't replay history.
	 */
	skipDatagramsToLatest() {
		this.state.datagrams.set([]);
	}

	/**
	 * Block until the next datagram arrives.
	 *
	 * `maxLatencyMs > 0` skips cache entries older than that bound. `0` means
	 * "no upper bound" (the cache eviction TTL still applies). For strict
	 * semantics call {@link skipDatagramsToLatest} once before the first
	 * {@link recvDatagram} call.
	 */
	async recvDatagram(maxLatencyMs: number): Promise<Datagram | undefined> {
		for (;;) {
			const now = performance.now();
			let result: Datagram | undefined;
			this.state.datagrams.mutate((slots) => {
				while (slots.length > 0) {
					const head = slots[0];
					const age = now - head.createdAt;
					if (age > MAX_DATAGRAM_AGE_MS) {
						slots.shift();
						continue;
					}
					if (maxLatencyMs > 0 && age > maxLatencyMs) {
						slots.shift();
						continue;
					}
					result = head.datagram;
					slots.shift();
					break;
				}
			});
			if (result) return result;

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			await Signal.race(this.state.datagrams, this.state.closed);
		}
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
	 * Closes the publisher and all associated groups.
	 */
	close(abort?: Error) {
		this.state.closed.set(abort ?? true);

		for (const group of this.state.groups.peek()) {
			group.close(abort);
		}
		// Datagrams have no per-entry close; just drop the cache so any waiting
		// consumer wakes and observes `closed`.
		this.state.datagrams.set([]);
	}
}
