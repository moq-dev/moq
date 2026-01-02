import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import type * as Catalog from "./catalog";
import * as Container from "./container";
import * as Time from "./time";

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

export function encode(source: Uint8Array | Source, timestamp: Time.Micro, container?: Catalog.Container): Uint8Array {
	// Encode timestamp using the specified container format
	const timestampBytes = Container.encodeTimestamp(timestamp, container);

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

// NOTE: A keyframe is always the first frame in a group, so it's not encoded on the wire.
export function decode(buffer: Uint8Array, container?: Catalog.Container): { data: Uint8Array; timestamp: Time.Micro } {
	// Decode timestamp using the specified container format
	const [timestamp, data] = Container.decodeTimestamp(buffer, container);
	return { timestamp: timestamp as Time.Micro, data };
}

export class Producer {
	#track: Moq.Track;
	#group?: Moq.Group;
	#container?: Catalog.Container;

	constructor(track: Moq.Track, container?: Catalog.Container) {
		this.#track = track;
		this.#container = container;
	}

	encode(data: Uint8Array | Source, timestamp: Time.Micro, keyframe: boolean) {
		if (keyframe) {
			this.#group?.close();
			this.#group = this.#track.appendGroup();
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		this.#group?.writeFrame(encode(data, timestamp, this.#container));
	}

	close() {
		this.#track.close();
		this.#group?.close();
	}
}

export interface ConsumerProps {
	// Target latency in milliseconds (default: 0)
	latency?: Signal<Time.Milli> | Time.Milli;
	container?: Catalog.Container;
}

interface Group {
	consumer: Moq.Group;
	frames: Frame[]; // decode order
	latest?: Time.Micro; // The timestamp of the latest known frame
}

export class Consumer {
	#track: Moq.Track;
	#latency: Signal<Time.Milli>;
	#container?: Catalog.Container;
	#groups: Group[] = [];
	#active?: number; // the active group sequence number

	// Wake up the consumer when a new frame is available.
	#notify?: () => void;

	#signals = new Effect();

	constructor(track: Moq.Track, props?: ConsumerProps) {
		this.#track = track;
		this.#latency = Signal.from(props?.latency ?? Time.Milli.zero);
		this.#container = props?.container;

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
		// For live streams (fmp4), start from the first group we receive (which should be the most recent available)
		// The init segment will be detected from the first frame of the active group
		
		for (;;) {
			console.log(`[Frame.Consumer] Waiting for next group, current active=${this.#active ?? 'undefined'}, totalGroups=${this.#groups.length}`);
			const consumer = await this.#track.nextGroup();
			if (!consumer) {
				console.log(`[Frame.Consumer] No more groups available (nextGroup returned null), breaking`);
				break;
			}

			console.log(`[Frame.Consumer] Received group: sequence=${consumer.sequence}, active=${this.#active ?? 'undefined'}, container=${this.#container ?? 'undefined'}, totalGroups=${this.#groups.length}`);

			// For fmp4 container (live streams), start from the first group we receive
			// This should be the most recent group available when we subscribe
			if (this.#container === "fmp4") {
				if (this.#active === undefined) {
					// First group - start from here (this is a live stream, so start from the most recent available)
					this.#active = consumer.sequence;
					console.log(`[Frame.Consumer] Starting from first received group (live stream): sequence=${consumer.sequence}, setting active=${this.#active}`);
				} else if (consumer.sequence < this.#active) {
					// Skip old groups (but accept groups equal to or greater than active)
					console.log(`[Frame.Consumer] Skipping old group in live stream: sequence=${consumer.sequence} < active=${this.#active}`);
					consumer.close();
					continue;
				} else if (consumer.sequence === this.#active && this.#groups.some(g => g.consumer.sequence === consumer.sequence)) {
					// Skip duplicate group (same sequence and already in groups)
					console.log(`[Frame.Consumer] Skipping duplicate group in live stream: sequence=${consumer.sequence} == active=${this.#active} and already in groups`);
					consumer.close();
					continue;
				} else {
					// New group or same sequence but not in groups yet - accept it and update active
					if (consumer.sequence > this.#active) {
						console.log(`[Frame.Consumer] New group in live stream: sequence=${consumer.sequence} > active=${this.#active}, accepting and updating active`);
						this.#active = consumer.sequence;
					} else {
						console.log(`[Frame.Consumer] Accepting group with same sequence as active: sequence=${consumer.sequence} == active=${this.#active} (not in groups yet)`);
					}
				}
			} else {
				// For non-fmp4 containers, use standard logic
				if (this.#active === undefined) {
					this.#active = consumer.sequence;
					console.log(`[Frame.Consumer] First group received: sequence=${consumer.sequence}, setting active=${this.#active}`);
				}

				if (consumer.sequence < this.#active) {
					console.warn(`[Frame.Consumer] Skipping old group: sequence=${consumer.sequence} < active=${this.#active}`);
					consumer.close();
					continue;
				}
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
			let frameCount = 0;

			console.log(`[Frame.Consumer] Starting to read frames from group ${group.consumer.sequence}, active=${this.#active ?? 'undefined'}`);

			for (;;) {
				console.log(`[Frame.Consumer] Calling readFrame() on group ${group.consumer.sequence}, frameCount=${frameCount}`);
				const next = await group.consumer.readFrame();
				if (!next) {
					console.log(`[Frame.Consumer] Group ${group.consumer.sequence} finished (readFrame returned null), read ${frameCount} frames total, active=${this.#active ?? 'undefined'}`);
					break;
				}

				frameCount++;
				const { data, timestamp } = decode(next, this.#container);
				const frame = {
					data,
					timestamp,
					keyframe,
					group: group.consumer.sequence,
				};

				console.log(`[Frame.Consumer] Read frame ${frameCount} from group ${group.consumer.sequence}: timestamp=${timestamp}, size=${data.byteLength}, keyframe=${keyframe}, active=${this.#active ?? 'undefined'}`);

				keyframe = false;

				group.frames.push(frame);

				if (!group.latest || timestamp > group.latest) {
					group.latest = timestamp;
				}

				if (group.consumer.sequence === this.#active) {
					console.log(`[Frame.Consumer] Notifying decoder that frame is available from active group ${group.consumer.sequence}`);
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
				const oldActive = this.#active;
				this.#active += 1;
				console.log(`[Frame.Consumer] Group ${oldActive} finished, advancing active to ${this.#active}, totalGroups=${this.#groups.length}`);

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
		if (latency < Time.Micro.fromMilli(this.#latency.peek())) return;

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
				if (frame) {
					console.log(`[Frame.Consumer] Returning frame from group ${this.#groups[0].consumer.sequence}, remaining frames in group=${this.#groups[0].frames.length}, active=${this.#active}`);
					return frame;
				}

				// Check if the group is done and then remove it.
				if (this.#active > this.#groups[0].consumer.sequence) {
					console.log(`[Frame.Consumer] Group ${this.#groups[0].consumer.sequence} is done (active=${this.#active}), removing from groups`);
					this.#groups.shift();
					continue;
				}
			}

			if (this.#notify) {
				throw new Error("multiple calls to decode not supported");
			}

			console.log(`[Frame.Consumer] No frames available, waiting for notify. active=${this.#active ?? 'undefined'}, groups=${this.#groups.length}, groupSequences=[${this.#groups.map(g => g.consumer.sequence).join(', ')}]`);

			const wait = new Promise<void>((resolve) => {
				this.#notify = resolve;
			}).then(() => {
				console.log(`[Frame.Consumer] Notified, checking for frames again. active=${this.#active ?? 'undefined'}, groups=${this.#groups.length}`);
				return true;
			});

			if (!(await Promise.race([wait, this.#signals.closed]))) {
				this.#notify = undefined;
				// Consumer was closed while waiting for a new frame.
				console.log(`[Frame.Consumer] Consumer closed while waiting`);
				return undefined;
			}
		}
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
