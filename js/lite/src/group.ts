import { Signal } from "@moq/signals";
import type { Frame } from "./frame";

export class GroupState {
	frames = new Signal<Frame[]>([]);
	closed = new Signal<boolean | Error>(false);
	total = new Signal<number>(0); // The total number of frames in the group thus far
}

export class Group {
	readonly sequence: number;

	state = new GroupState();
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
	writeFrame(frame: Frame) {
		if (this.state.closed.peek()) throw new Error("group is closed");

		this.state.frames.mutate((frames) => {
			frames.push(frame);
		});

		this.state.total.update((total) => total + 1);
	}

	/**
	 * Reads the next frame from the group.
	 * @returns A promise that resolves to the next frame or undefined
	 */
	async readFrame(): Promise<Frame | undefined> {
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

	async readFrameSequence(): Promise<{ sequence: number; frame: Frame } | undefined> {
		for (;;) {
			const frames = this.state.frames.peek();
			const frame = frames.shift();
			if (frame) return { sequence: this.state.total.peek() - frames.length - 1, frame };

			const closed = this.state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return;

			await Signal.race(this.state.frames, this.state.closed);
		}
	}

	close(abort?: Error) {
		this.state.closed.set(abort ?? true);
	}
}
