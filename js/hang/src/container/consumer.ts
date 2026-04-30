import type { Time } from "@moq/lite";
import * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";

import type { ContainerFormat } from "./format";
import type { BufferedRanges, Frame } from "./types";

export interface ConsumerProps {
	format: ContainerFormat;
	latency?: Signal<Time.Milli> | Time.Milli;
	/** Returns the PTS that should be rendering right now, or undefined if unknown. */
	now?: () => Time.Milli | undefined;
	/** When false, frames are delivered from any group as soon as available (no inter-group serialization).
	 *  Useful for audio where every frame is independently decodable. Default: true. */
	sequential?: boolean;
}

interface Group {
	consumer: Moq.Group;
	frames: Frame[];
	latest?: Time.Micro;
	done?: boolean;
}

export class Consumer {
	#track: Moq.Track;
	#format: ContainerFormat;
	#latency: Signal<Time.Milli>;
	#now?: () => Time.Milli | undefined;
	#sequential: boolean;
	#groups: Group[] = [];
	#active?: number;

	#notify?: () => void;

	#buffered = new Signal<BufferedRanges>([]);
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#signals = new Effect();

	constructor(track: Moq.Track, props: ConsumerProps) {
		this.#track = track;
		this.#format = props.format;
		this.#latency = Signal.from(props.latency ?? Moq.Time.Milli.zero);
		this.#now = props.now;
		this.#sequential = props.sequential ?? true;

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
		for (;;) {
			const consumer = await this.#track.recvGroup();
			if (!consumer) break;

			if (this.#active === undefined) {
				this.#active = consumer.sequence;
			}

			if (consumer.sequence < this.#active) {
				console.warn(`skipping old group: ${consumer.sequence} < ${this.#active}`);
				consumer.close();
				continue;
			}

			const group: Group = {
				consumer,
				frames: [],
			};

			this.#groups.push(group);
			this.#groups.sort((a, b) => a.consumer.sequence - b.consumer.sequence);

			this.#signals.spawn(this.#runGroup.bind(this, group));
		}
	}

	async #runGroup(group: Group) {
		try {
			let index = 0;

			for (;;) {
				const raw = await group.consumer.readFrame();
				if (!raw) break;

				const decoded = this.#format.decode(raw);

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

					if (!this.#sequential || group.consumer.sequence === this.#active) {
						this.#notify?.();
						this.#notify = undefined;
					} else {
						this.#checkLatency();
					}
				}
			}
		} catch (_err) {
			// Drop the entire group on any decode error.
			group.frames.length = 0;
			group.latest = undefined;
		} finally {
			group.done = true;

			if (group.consumer.sequence === this.#active) {
				this.#active += 1;
			}

			this.#updateBuffered();

			this.#notify?.();
			this.#notify = undefined;

			group.consumer.close();
		}
	}

	#checkLatency() {
		if (this.#active === undefined) return;

		let skipped = false;

		while (this.#groups.length > 0) {
			const first = this.#groups[0];
			const firstPts = first.frames.at(0)?.timestamp;
			if (firstPts === undefined) break;

			const threshold = this.#latency.peek();

			// Use wall-clock playback position when available.
			// This avoids false skips when frames arrive faster than real-time
			// (e.g. CMAF groups that dump all frames synchronously).
			const now = this.#now?.();
			if (now !== undefined) {
				const ptsMilli = Moq.Time.Milli.fromMicro(firstPts);
				if (Moq.Time.Milli.sub(now, ptsMilli) <= threshold) break;
			} else {
				// PTS-span fallback needs at least two groups to establish span.
				if (this.#groups.length < 2) break;

				const thresholdMicro = Moq.Time.Micro.fromMilli(threshold);

				let max: number | undefined;
				for (const group of this.#groups) {
					if (group.latest !== undefined && (max === undefined || group.latest > max)) {
						max = group.latest;
					}
				}

				if (max === undefined) break;
				if (max - firstPts <= thresholdMicro) break;
			}

			const removed = this.#groups.shift();
			if (!removed) break;
			this.#active = this.#groups[0]?.consumer.sequence;
			console.warn(`skipping slow group: ${removed.consumer.sequence} -> ${this.#active}`);

			removed.consumer.close();
			removed.frames.length = 0;
			skipped = true;
		}

		if (skipped) {
			this.#updateBuffered();

			this.#notify?.();
			this.#notify = undefined;
		}
	}

	async next(): Promise<{ frame: Frame | undefined; group: number } | undefined> {
		for (;;) {
			if (this.#sequential) {
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
			} else {
				// Unordered: return the lowest-PTS frame from any group.
				// Clean up done+empty groups first.
				while (this.#groups.length > 0 && this.#groups[0].done && this.#groups[0].frames.length === 0) {
					this.#groups.shift();
					this.#active = this.#groups[0]?.consumer.sequence;
				}

				// Find the group with the lowest-PTS next frame.
				let best: Group | undefined;
				for (const group of this.#groups) {
					const f = group.frames[0];
					if (!f) continue;
					if (!best || f.timestamp < best.frames[0].timestamp) {
						best = group;
					}
				}

				if (best) {
					const frame = best.frames.shift();
					if (frame) {
						this.#updateBuffered();
						return { frame, group: best.consumer.sequence };
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
