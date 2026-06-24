import { Signal } from "@moq/signals";

/** Reactive backing state for a {@link Group}: buffered frames, a closed flag, and the running frame count. */
class GroupState {
	frames = new Signal<Uint8Array[]>([]);
	closed = new Signal<boolean | Error>(false);
	total = new Signal<number>(0); // The total number of frames in the group thus far
}

/** An ordered stream of frames within a track, delivered over a single QUIC stream. */
export class Group {
	/** Sequence number of this group within its track. */
	readonly sequence: number;

	// Reactive backing state, deliberately private: read through the read* methods so callers can't
	// poke the signals directly (and so the internal representation can change).
	#state = new GroupState();

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	constructor(sequence: number) {
		this.sequence = sequence;

		// Cache the closed promise to avoid recreating it every time.
		this.closed = new Promise((resolve) => {
			const dispose = this.#state.closed.subscribe((closed) => {
				if (!closed) return;
				resolve(closed instanceof Error ? closed : undefined);
				dispose();
			});
		});
	}

	/**
	 * Writes a frame to the group.
	 * @param frame - The frame to write
	 */
	writeFrame(frame: Uint8Array) {
		if (this.#state.closed.peek()) throw new Error("group is closed");

		this.#state.frames.mutate((frames) => {
			frames.push(frame);
		});

		this.#state.total.update((total) => total + 1);
	}

	/** Write a string as a single UTF-8 encoded frame. */
	writeString(str: string) {
		this.writeFrame(new TextEncoder().encode(str));
	}

	/** Write a value as a single JSON-encoded frame. */
	writeJson(json: unknown) {
		this.writeString(JSON.stringify(json));
	}

	/** Write a boolean as a single one-byte frame. */
	writeBool(bool: boolean) {
		this.writeFrame(new Uint8Array([bool ? 1 : 0]));
	}

	/** True once no further frames can be read: the group has closed and every buffered frame is read. */
	get done(): boolean {
		return this.#state.frames.peek().length === 0 && this.#state.closed.peek() !== false;
	}

	/**
	 * Reads the next already-buffered frame without blocking.
	 *
	 * Returns `undefined` when nothing is buffered right now. That is *not* by itself end-of-group:
	 * check {@link done} to tell "no frame buffered yet" (more may arrive) from "the group finished".
	 * Drain a backlog by looping until this returns `undefined`, then branch on {@link done}: if not
	 * done, {@link readable} resolves when the next frame arrives.
	 */
	tryReadFrame(): Uint8Array | undefined {
		return this.tryReadFrameSequence()?.data;
	}

	/** Like {@link tryReadFrame} but also reports the frame's sequence number within the group. */
	tryReadFrameSequence(): { sequence: number; data: Uint8Array } | undefined {
		const frames = this.#state.frames.peek();
		const data = frames.shift();
		if (data === undefined) return undefined;
		return { sequence: this.#state.total.peek() - frames.length - 1, data };
	}

	/**
	 * Resolves once {@link readFrame} would not block: a frame is buffered, or the group has closed.
	 * Always settles (never hangs), so on a finished group it resolves immediately; pair it with
	 * {@link done} to avoid re-waiting on a group that has nothing left.
	 *
	 * Lets a caller fold "this group has a frame" into a larger wait (e.g. racing it against a new
	 * group arriving) without touching the group's internal signals.
	 */
	async readable(): Promise<void> {
		for (;;) {
			if (this.#state.frames.peek().length > 0) return;
			if (this.#state.closed.peek()) return;
			await Signal.race(this.#state.frames, this.#state.closed);
		}
	}

	/**
	 * Reads the next frame from the group.
	 * @returns A promise that resolves to the next frame or undefined
	 */
	async readFrame(): Promise<Uint8Array | undefined> {
		return (await this.readFrameSequence())?.data;
	}

	/** Reads the next frame along with its sequence number within the group. */
	async readFrameSequence(): Promise<{ sequence: number; data: Uint8Array } | undefined> {
		for (;;) {
			const next = this.tryReadFrameSequence();
			if (next) return next;

			// Drain buffered frames before observing the close, so a closed group still yields them.
			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			await this.readable();
		}
	}

	/** Reads the next frame and decodes it as a UTF-8 string. */
	async readString(): Promise<string | undefined> {
		const frame = await this.readFrame();
		return frame ? new TextDecoder().decode(frame) : undefined;
	}

	/** Reads the next frame and parses it as JSON. */
	async readJson(): Promise<unknown | undefined> {
		const frame = await this.readString();
		return frame ? JSON.parse(frame) : undefined;
	}

	/** Reads the next frame and decodes it as a one-byte boolean. */
	async readBool(): Promise<boolean | undefined> {
		const frame = await this.readFrame();
		return frame ? frame[0] === 1 : undefined;
	}

	/** Closes the group, optionally with an error to abort readers. */
	close(abort?: Error) {
		this.#state.closed.set(abort ?? true);
	}
}
