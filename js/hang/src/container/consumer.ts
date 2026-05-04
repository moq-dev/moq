import type { Time } from "@moq/lite";
import * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";

import type { Format } from "./format";
import type { BufferedRanges, Frame } from "./types";

export interface ConsumerProps {
	format: Format;
	// Target latency in milliseconds (default: 0)
	latency?: Signal<Time.Milli> | Time.Milli;
}

interface Group {
	consumer: Moq.Group;
	frames: Frame[]; // decode order
	latest?: Time.Micro; // The timestamp of the latest known frame
	done?: boolean; // Set when #runGroup finishes reading all frames
}

export class Consumer {
	#track: Moq.Track;
	#format: Format;
	#latency: Signal<Time.Milli>;
	#groups: Group[] = [];
	#active?: number; // the active group sequence number

	// Wake up the consumer when a new frame is available.
	#notify?: () => void;

	#buffered = new Signal<BufferedRanges>([]);
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#signals = new Effect();

	constructor(track: Moq.Track, props: ConsumerProps) {
		this.#track = track;
		this.#format = props.format;
		this.#latency = Signal.from(props.latency ?? Moq.Time.Milli.zero);

		this.#signals.spawn(this.#run.bind(this));
		this.#signals.cleanup(() => {
			this.#track.close();
			for (const group of this.#groups) {
				group.consumer.close();
			}
			this.#groups.length = 0;
		});
	}

	async #run() {
		// Start fetching groups in the background
		for (;;) {
			const consumer = await this.#track.recvGroup();
			if (!consumer) break;

			// To improve TTV, we always start with the first group.
			// For higher latencies we might need to figure something else out, as its racey.
			if (this.#active === undefined) {
				this.#active = consumer.sequence;
			}

			if (consumer.sequence < this.#active) {
				console.warn(`skipping old group: ${consumer.sequence} < ${this.#active}`);
				// Skip old groups.
				consumer.close();
				continue;
			}

			const group: Group = {
				consumer,
				frames: [],
			};

			// Insert into #groups based on the group sequence number (ascending).
			// This is used to cancel old groups.
			this.#groups.push(group);
			this.#groups.sort((a, b) => a.consumer.sequence - b.consumer.sequence);

			// Start buffering frames from this group
			this.#signals.spawn(this.#runGroup.bind(this, group));
		}
	}

	async #runGroup(group: Group) {
		try {
			let index = 0;

			for (;;) {
				const next = await group.consumer.readFrame();
				if (!next) break;

				const decoded = this.#format.decode(next);

				for (const sample of decoded) {
					const frame: Frame = {
						data: sample.data,
						timestamp: sample.timestamp,
						// Protocol invariant: groups always start at a keyframe.
						// For index 0, we enforce this regardless of what the format reports.
						// For index > 0, we trust the format's keyframe detection.
						keyframe: index === 0 ? true : sample.keyframe,
					};

					index++;

					group.frames.push(frame);

					if (group.latest === undefined || frame.timestamp > group.latest) {
						group.latest = frame.timestamp;
					}

					this.#updateBuffered();

					if (group.consumer.sequence === this.#active) {
						this.#notify?.();
						this.#notify = undefined;
					} else {
						// Check for latency violations if this is a newer group.
						this.#checkLatency();
					}
				}
			}
		} catch (_err) {
			// Stop reading the group but keep already-decoded frames.
			// A decode error or stream RESET truncates the tail of the GoP;
			// frames decoded before the error are still valid and playable.
		} finally {
			group.done = true;

			if (group.consumer.sequence === this.#active) {
				// Advance to the next group.
				this.#active += 1;
			}

			// Recompute buffered ranges now that this group is done,
			// so consecutive done groups can merge into a single range.
			this.#updateBuffered();

			// Always notify - the consumer may need to advance past this group
			// even if it wasn't active when this task finished.
			this.#notify?.();
			this.#notify = undefined;

			group.consumer.close();
		}
	}

	#checkLatency() {
		if (this.#active === undefined) return;

		let skipped = false;

		// Keep skipping the oldest group while the buffered span exceeds the latency target.
		// This also handles gaps in group sequence numbers: if #active points to a missing
		// group, the latency span proves the missing content is too old to wait for.
		while (this.#groups.length >= 2) {
			const threshold = Moq.Time.Micro.fromMilli(this.#latency.peek());

			// Check the difference between the earliest and latest known frames.
			let min: number | undefined;
			let max: number | undefined;

			for (const group of this.#groups) {
				if (group.latest === undefined) continue;

				const frame = group.frames.at(0)?.timestamp ?? group.latest;
				if (min === undefined || frame < min) min = frame;
				if (max === undefined || group.latest > max) max = group.latest;
			}

			if (min === undefined || max === undefined) break;

			const latency = max - min;
			if (latency <= threshold) break;

			const first = this.#groups.shift();
			if (!first) break;
			this.#active = this.#groups[0]?.consumer.sequence;
			console.warn(`skipping slow group: ${first.consumer.sequence} -> ${this.#active}`);

			first.consumer.close();
			first.frames.length = 0;
			skipped = true;
		}

		if (skipped) {
			this.#updateBuffered();

			// Wake up any consumers waiting for a new frame.
			this.#notify?.();
			this.#notify = undefined;
		}
	}

	// Returns the next frame in order, along with the group number.
	// If frame is undefined, the group is done.
	async next(): Promise<{ frame: Frame | undefined; group: number } | undefined> {
		for (;;) {
			if (
				this.#groups.length > 0 &&
				this.#active !== undefined &&
				this.#groups[0].consumer.sequence <= this.#active
			) {
				const frame = this.#groups[0].frames.shift();
				if (frame) {
					this.#updateBuffered();
					return { frame, group: this.#groups[0].consumer.sequence };
				}

				// Check if the group is done and then remove it.
				// A group is removable when #active has advanced past it, OR when
				// its #runGroup task has finished (done) and all frames are consumed.
				// The latter handles the case where #runGroup finished before
				// #active reached this group (e.g. after a latency skip).
				if (this.#active > this.#groups[0].consumer.sequence || this.#groups[0].done) {
					if (this.#groups[0].consumer.sequence === this.#active) {
						this.#active += 1;
					}

					const group = this.#groups.shift();
					if (group) {
						this.#updateBuffered();
						return { frame: undefined, group: group.consumer.sequence };
					}
				}
			}

			if (this.#notify) {
				throw new Error("multiple calls to next not supported");
			}

			const wait = new Promise<void>((resolve) => {
				this.#notify = resolve;
			}).then(() => true);

			if (!(await Promise.race([wait, this.#signals.closed]))) {
				this.#notify = undefined;
				// Consumer was closed while waiting for a new frame.
				return undefined;
			}
		}
	}

	#updateBuffered(): void {
		const ranges: BufferedRanges = [];

		let prev: Group | undefined;

		for (const group of this.#groups) {
			const first = group.frames.at(0);
			if (!first || group.latest === undefined) continue;

			const start = Moq.Time.Milli.fromMicro(first.timestamp);
			const end = Moq.Time.Milli.fromMicro(group.latest);

			const last = ranges.at(-1);
			const contiguous = prev?.done && prev.consumer.sequence + 1 === group.consumer.sequence;
			if (last && (last.end >= start || contiguous)) {
				last.end = Moq.Time.Milli.max(last.end, end);
			} else {
				ranges.push({ start, end });
			}

			prev = group;
		}

		this.#buffered.set(ranges);
	}

	close(): void {
		this.#signals.close();
	}
}
