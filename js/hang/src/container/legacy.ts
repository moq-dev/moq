import type { Time } from "@moq/lite";
import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";

export interface Source {
	byteLength: number;
	copyTo(buffer: Uint8Array): void;
}

export interface Frame {
	data: Uint8Array;
	timestamp: Time.Micro;
	keyframe: boolean;
	group: number;
}

// A Helper class to encode frames into a track.
export class Producer {
	#track: Moq.Track;
	#group?: Moq.Group;

	constructor(track: Moq.Track) {
		this.#track = track;
	}

	encode(data: Uint8Array | Source, timestamp: Time.Micro, keyframe: boolean) {
		if (keyframe) {
			this.#group?.close();
			this.#group = this.#track.appendGroup();
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		this.#group?.writeFrame(Producer.#encode(data, timestamp));
	}

	static #encode(source: Uint8Array | Source, timestamp: Time.Micro): Uint8Array {
		const timestampBytes = Moq.Varint.encode(timestamp);

		// Allocate buffer for timestamp + payload
		const payloadSize = source instanceof Uint8Array ? source.byteLength : source.byteLength;
		const data = new Uint8Array(timestampBytes.byteLength + payloadSize);

		// Write timestamp header
		data.set(timestampBytes, 0);

		// Write payload
		if (source instanceof Uint8Array) {
			data.set(source, timestampBytes.byteLength);
		} else {
			source.copyTo(data.subarray(timestampBytes.byteLength));
		}

		return data;
	}

	close(err?: Error) {
		this.#track.close(err);
		this.#group?.close();
	}
}

export interface ConsumerProps {
	// Target latency in milliseconds (default: 0)
	latency?: Signal<Time.Milli> | Time.Milli;
}

interface Group {
	consumer: Moq.Group;
	frames: Frame[]; // decode order
	latest?: Time.Micro; // The timestamp of the latest known frame
}

export class Consumer {
	#track: Moq.Track;
	#latency: Signal<Time.Milli>;
	#groups: Group[] = [];
	#active?: number; // the active group sequence number

	// Wake up the consumer when a new frame is available.
	#notify?: () => void;

	#signals = new Effect();

	constructor(track: Moq.Track, props?: ConsumerProps) {
		this.#track = track;
		this.#latency = Signal.from(props?.latency ?? Moq.Time.Milli.zero);

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
			const consumer = await this.#track.nextGroup();
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

			const group = {
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
			let keyframe = true;

			for (;;) {
				const next = await group.consumer.readFrame();
				if (!next) break;

				const { data, timestamp } = Consumer.#decode(next);
				const frame = {
					data,
					timestamp,
					keyframe,
					group: group.consumer.sequence,
				};

				keyframe = false;

				group.frames.push(frame);

				if (!group.latest || timestamp > group.latest) {
					group.latest = timestamp;
				}

				if (group.consumer.sequence === this.#active) {
					this.#notify?.();
					this.#notify = undefined;
				} else {
					// Check for latency violations if this is a newer group.
					this.#checkLatency();
				}
			}
		} catch (_err) {
			// Ignore errors, we close groups on purpose to skip them.
		} finally {
			if (group.consumer.sequence === this.#active) {
				// Advance to the next group.
				this.#active += 1;

				this.#notify?.();
				this.#notify = undefined;
			}

			group.consumer.close();
		}
	}

	#checkLatency() {
		// We can only skip if there are at least two groups.
		if (this.#groups.length < 2) return;

		const first = this.#groups[0];

		// Check the difference between the earliest known frame and the latest known frame
		let min: number | undefined;
		let max: number | undefined;

		for (const group of this.#groups) {
			if (!group.latest) continue;

			// Use the earliest unconsumed frame in the group.
			const frame = group.frames.at(0)?.timestamp ?? group.latest;
			if (min === undefined || frame < min) {
				min = frame;
			}

			if (max === undefined || group.latest > max) {
				max = group.latest;
			}
		}

		if (min === undefined || max === undefined) return;

		const latency = max - min;
		if (latency < Moq.Time.Micro.fromMilli(this.#latency.peek())) return;

		if (this.#active !== undefined && first.consumer.sequence <= this.#active) {
			this.#groups.shift();

			console.warn(`skipping slow group: ${first.consumer.sequence} < ${this.#groups[0]?.consumer.sequence}`);

			first.consumer.close();
			first.frames.length = 0;
		}

		// Advance to the next known group.
		// NOTE: Can't be undefined, because we checked above.
		this.#active = this.#groups[0]?.consumer.sequence;

		// Wake up any consumers waiting for a new frame.
		this.#notify?.();
		this.#notify = undefined;
	}

	async decode(): Promise<Frame | undefined> {
		for (;;) {
			if (
				this.#groups.length > 0 &&
				this.#active !== undefined &&
				this.#groups[0].consumer.sequence <= this.#active
			) {
				const frame = this.#groups[0].frames.shift();
				if (frame) return frame;

				// Check if the group is done and then remove it.
				if (this.#active > this.#groups[0].consumer.sequence) {
					this.#groups.shift();
					continue;
				}
			}

			if (this.#notify) {
				throw new Error("multiple calls to decode not supported");
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

	// NOTE: A keyframe is always the first frame in a group, so it's not encoded on the wire.
	static #decode(buffer: Uint8Array): { data: Uint8Array; timestamp: Time.Micro } {
		const [timestamp, data] = Moq.Varint.decode(buffer);
		return { timestamp: timestamp as Time.Micro, data };
	}

	close(): void {
		this.#signals.close();

		for (const group of this.#groups) {
			group.consumer.close();
			group.frames.length = 0;
		}

		this.#groups.length = 0;
	}
}
