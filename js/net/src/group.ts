import { Signal } from "@moq/signals";

/** Reactive backing state for a {@link Group}: buffered frames, a closed flag, and the running frame count. */
export class GroupState {
	frames = new Signal<Uint8Array[]>([]);
	closed = new Signal<boolean | Error>(false);
	total = new Signal<number>(0); // The total number of frames in the group thus far
}

/** An ordered stream of frames within a track, delivered over a single QUIC stream. */
export class Group {
	/** Sequence number of this group within its track. */
	readonly sequence: number;

	/** Reactive backing state. */
	state = new GroupState();

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	constructor(sequence: number) {
		this.sequence = sequence;

		// Cache the closed promise to avoid recreating it every time.
		this.closed = new Promise((resolve) => {
			const dispose = this.state.closed.subscribe((closed) => {
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
		if (this.state.closed.peek()) throw new Error("group is closed");

		this.state.frames.mutate((frames) => {
			frames.push(frame);
		});

		this.state.total.update((total) => total + 1);
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

	/**
	 * Reads the next frame from the group.
	 * @returns A promise that resolves to the next frame or undefined
	 */
	async readFrame(): Promise<Uint8Array | undefined> {
		for (;;) {
			const frames = this.state.frames.peek();
			const frame = frames.shift();
			if (frame) return frame;

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return;

			await Signal.race(this.state.frames, this.state.closed);
		}
	}

	/** Reads the next frame along with its sequence number within the group. */
	async readFrameSequence(): Promise<{ sequence: number; data: Uint8Array } | undefined> {
		for (;;) {
			const frames = this.state.frames.peek();
			const frame = frames.shift();
			if (frame) return { sequence: this.state.total.peek() - frames.length - 1, data: frame };

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return;

			await Signal.race(this.state.frames, this.state.closed);
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
		this.state.closed.set(abort ?? true);
	}
}
