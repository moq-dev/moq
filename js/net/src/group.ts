import { Signal } from "@moq/signals";

export class GroupState {
	frames = new Signal<Uint8Array[]>([]);
	closed = new Signal<boolean | Error>(false);
	total = new Signal<number>(0); // The total number of frames in the group thus far
}

export class Group {
	readonly sequence: number;

	state = new GroupState();
	readonly closed: Promise<Error | undefined>;

	// Downstream copies that receive every frame written here, synchronously. Used by
	// TrackProducer to fan one source group out to per-subscriber groups.
	#mirrors?: Set<Group>;

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

		// Tee into live mirrors, dropping any the consumer has already closed.
		if (this.#mirrors) {
			for (const mirror of this.#mirrors) {
				if (mirror.state.closed.peek()) this.#mirrors.delete(mirror);
				else mirror.writeFrame(frame);
			}
		}
	}

	/**
	 * Create an independent copy that receives every frame written to this group.
	 *
	 * Frames written so far are replayed synchronously; later writes (and the close)
	 * are teed in as they happen. The copy has its own read cursor, so consumers never
	 * steal frames from each other. Internal to {@link TrackProducer} fan-out.
	 */
	mirror(): Group {
		const dst = new Group(this.sequence);
		for (const frame of this.state.frames.peek()) dst.writeFrame(frame);

		const closed = this.state.closed.peek();
		if (closed) {
			dst.close(closed instanceof Error ? closed : undefined);
			return dst;
		}

		this.#mirrors ??= new Set();
		this.#mirrors.add(dst);
		return dst;
	}

	writeString(str: string) {
		this.writeFrame(new TextEncoder().encode(str));
	}

	writeJson(json: unknown) {
		this.writeString(JSON.stringify(json));
	}

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

	async readString(): Promise<string | undefined> {
		const frame = await this.readFrame();
		return frame ? new TextDecoder().decode(frame) : undefined;
	}

	async readJson(): Promise<unknown | undefined> {
		const frame = await this.readString();
		return frame ? JSON.parse(frame) : undefined;
	}

	async readBool(): Promise<boolean | undefined> {
		const frame = await this.readFrame();
		return frame ? frame[0] === 1 : undefined;
	}

	close(abort?: Error) {
		this.state.closed.set(abort ?? true);

		if (this.#mirrors) {
			for (const mirror of this.#mirrors) mirror.close(abort);
			this.#mirrors.clear();
		}
	}
}
