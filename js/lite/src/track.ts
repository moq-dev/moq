import { Signal } from "@moq/signals";
import { Group } from "./group.ts";
import { Time } from "./index.ts";

export interface TrackProps {
	name: string;
	priority?: number | Signal<number>;
	maxLatency?: Time.Milli | Signal<Time.Milli>;
}

export class Track {
	readonly name: string;

	#groups = new Signal<Group[]>([]);
	maxLatency: Signal<Time.Milli>;
	priority = new Signal<number>(0);

	#closed = new Signal<boolean | Error>(false);
	readonly closed: Promise<Error | undefined>;

	#next?: number;

	constructor(props: TrackProps) {
		this.name = props.name;
		this.priority = Signal.from(props.priority ?? 0);
		this.maxLatency = Signal.from(props.maxLatency ?? Time.Milli.zero);

		this.closed = new Promise((resolve) => {
			const dispose = this.#closed.watch((closed) => {
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
		if (this.#closed.peek()) throw new Error("track is closed");

		const group = new Group(this.#next ?? 0);

		this.#next = group.sequence + 1;
		this.#groups.mutate((groups) => {
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
		if (this.#closed.peek()) throw new Error("track is closed");

		if (group.sequence < (this.#next ?? 0)) {
			group.close();
			return;
		}

		this.#next = group.sequence + 1;
		this.#groups.mutate((groups) => {
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

	async nextGroup(): Promise<Group | undefined> {
		for (;;) {
			const groups = this.#groups.peek();
			if (groups.length > 0) {
				return groups.shift();
			}

			const closed = this.#closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			await Signal.race(this.#groups, this.#closed);
		}
	}

	async readFrame(): Promise<Uint8Array | undefined> {
		return (await this.readFrameSequence())?.data;
	}

	// Returns the sequence number of the group and frame, not just the data.
	async readFrameSequence(): Promise<{ group: number; frame: number; data: Uint8Array } | undefined> {
		for (;;) {
			const groups = this.#groups.peek();

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
				const closed = this.#closed.peek();
				if (closed instanceof Error) throw closed;
				if (closed) return undefined;

				await Signal.race(this.#groups, this.#closed);
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
			const closed = this.#closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			// NOTE: We don't care if the latest group was closed or not.
			await Signal.race(this.#groups, this.#closed, group.state.frames);
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
		this.#closed.set(abort ?? true);

		for (const group of this.#groups.peek()) {
			group.close(abort);
		}
	}
}
