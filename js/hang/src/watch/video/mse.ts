import * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";
import * as Container from "../../container";
import { type BufferedRanges, timeRangesToArray } from "../backend";
import type { Muxer } from "../mse";
import type { Backend, Stats } from "./backend";
import type { Source } from "./source";

/**
 * MSE-based video source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 */
export class Mse implements Backend {
	muxer: Muxer;
	source: Source;

	// TODO implement stats
	#stats = new Signal<Stats | undefined>(undefined);
	readonly stats: Getter<Stats | undefined> = this.#stats;

	#buffered = new Signal<BufferedRanges>([]);
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#stalled = new Signal<boolean>(false);
	readonly stalled: Getter<boolean> = this.#stalled;

	#timestamp = new Signal<Moq.Time.Milli>(Moq.Time.Milli.zero);
	readonly timestamp: Getter<Moq.Time.Milli> = this.#timestamp;

	signals = new Effect();

	constructor(muxer: Muxer, source: Source) {
		this.muxer = muxer;
		this.source = source;
		this.source.supported.set(supported); // super hacky

		this.signals.effect(this.#runMedia.bind(this));
		this.signals.effect(this.#runStalled.bind(this));
		this.signals.effect(this.#runTimestamp.bind(this));
	}

	#runMedia(effect: Effect): void {
		const element = effect.get(this.muxer.element);
		if (!element) return;

		const mediaSource = effect.get(this.muxer.mediaSource);
		if (!mediaSource) return;

		const broadcast = effect.get(this.source.broadcast);
		if (!broadcast) return;

		const active = effect.get(broadcast.active);
		if (!active) return;

		const track = effect.get(this.source.track);
		if (!track) return;

		const config = effect.get(this.source.config);
		if (!config) return;

		const mime = `video/mp4; codecs="${config.codec}"`;

		const sourceBuffer = mediaSource.addSourceBuffer(mime);
		effect.cleanup(() => {
			mediaSource.removeSourceBuffer(sourceBuffer);
			sourceBuffer.abort();
		});

		effect.event(sourceBuffer, "error", (e) => {
			console.error("[MSE] SourceBuffer error:", e);
		});

		effect.event(sourceBuffer, "updateend", () => {
			this.#buffered.set(timeRangesToArray(sourceBuffer.buffered));
		});

		if (config.container.kind === "cmaf") {
			this.#runCmafMedia(effect, active, track, config, sourceBuffer, element);
		} else {
			this.#runLegacyMedia(effect, active, track, config, sourceBuffer, element);
		}
	}

	async #appendBuffer(sourceBuffer: SourceBuffer, buffer: Uint8Array): Promise<void> {
		while (sourceBuffer.updating) {
			await new Promise((resolve) => sourceBuffer.addEventListener("updateend", resolve, { once: true }));
		}

		sourceBuffer.appendBuffer(buffer as BufferSource);

		while (sourceBuffer.updating) {
			await new Promise((resolve) => sourceBuffer.addEventListener("updateend", resolve, { once: true }));
		}
	}

	#runCmafMedia(
		effect: Effect,
		active: Moq.Broadcast,
		track: string,
		config: Catalog.VideoConfig,
		sourceBuffer: SourceBuffer,
		element: HTMLMediaElement,
	): void {
		if (config.container.kind !== "cmaf") throw new Error("unreachable");

		const data = active.subscribe(track, Catalog.PRIORITY.video);
		effect.cleanup(() => data.close());

		const timescale = config.container.timescale;

		effect.spawn(async () => {
			// Generate init segment from catalog config (uses track_id from container)
			const initSegment = Container.Cmaf.createVideoInitSegment(config);
			await this.#appendBuffer(sourceBuffer, initSegment);

			for (;;) {
				// TODO: Use Frame.Consumer for CMAF so we can support higher latencies.
				// It requires extracting the timestamp from the frame payload.
				const frame = await data.readFrame();
				if (!frame) return;

				// Extract the timestamp from the CMAF segment and mark when we received it.
				const timestamp = Container.Cmaf.decodeTimestamp(frame, timescale);
				this.source.sync.received(Moq.Time.Milli.fromMicro(timestamp));

				await this.#appendBuffer(sourceBuffer, frame);

				// Seek to the start of the buffer if we're behind it (for startup).
				if (element.buffered.length > 0 && element.currentTime < element.buffered.start(0)) {
					element.currentTime = element.buffered.start(0);
				}
			}
		});
	}

	#runLegacyMedia(
		effect: Effect,
		active: Moq.Broadcast,
		track: string,
		config: Catalog.VideoConfig,
		sourceBuffer: SourceBuffer,
		element: HTMLMediaElement,
	): void {
		const data = active.subscribe(track, Catalog.PRIORITY.video);
		effect.cleanup(() => data.close());

		// Create consumer that reorders groups/frames up to the provided latency.
		// Legacy container uses microsecond timescale implicitly.
		const consumer = new Container.Legacy.Consumer(data, {
			latency: this.source.sync.latency,
		});
		effect.cleanup(() => consumer.close());

		effect.spawn(async () => {
			// Generate init segment from catalog config (timescale = 1,000,000 = microseconds)
			const initSegment = Container.Cmaf.createVideoInitSegment(config);
			await this.#appendBuffer(sourceBuffer, initSegment);

			let sequence = 1;
			let duration: Moq.Time.Micro | undefined;

			// Buffer one frame so we can compute accurate duration from the next frame's timestamp
			let pending: Container.Legacy.Frame;
			for (;;) {
				const next = await consumer.next();
				if (!next) return;
				if (!next.frame) continue; // Skip over group done notifications.

				pending = next.frame;

				// Mark that we received this frame right now.
				this.source.sync.received(Moq.Time.Milli.fromMicro(pending.timestamp as Moq.Time.Micro));

				break;
			}

			for (;;) {
				const next = await consumer.next();
				if (next && !next.frame) continue; // Skip over group done notifications.
				const frame = next?.frame;

				// Compute duration from next frame's timestamp, or use last known duration if stream ended
				if (frame) {
					duration = Moq.Time.Micro.sub(frame.timestamp, pending.timestamp);

					// Mark that we received this frame right now for latency calculation.
					this.source.sync.received(Moq.Time.Milli.fromMicro(frame.timestamp as Moq.Time.Micro));
				}

				// Wrap raw frame in moof+mdat
				const segment = Container.Cmaf.encodeDataSegment({
					data: pending.data,
					timestamp: pending.timestamp,
					duration: duration ?? 0, // Default to 0 duration if there's literally one frame then stream FIN.
					keyframe: pending.keyframe,
					sequence: sequence++,
				});

				await this.#appendBuffer(sourceBuffer, segment);

				// Seek to the start of the buffer if we're behind it (for startup).
				if (element.buffered.length > 0 && element.currentTime < element.buffered.start(0)) {
					element.currentTime = element.buffered.start(0);
				}

				if (!frame) return;
				pending = frame;
			}
		});
	}

	#runStalled(effect: Effect): void {
		const element = effect.get(this.muxer.element);
		if (!element) return;

		const update = () => {
			this.#stalled.set(element.readyState <= HTMLMediaElement.HAVE_CURRENT_DATA);
		};

		// Set initial state
		update();

		// TODO Are these the correct events to use?
		effect.event(element, "waiting", update);
		effect.event(element, "playing", update);
		effect.event(element, "seeking", update);
	}

	#runTimestamp(effect: Effect): void {
		const element = effect.get(this.muxer.element);
		if (!element) return;

		// Use requestVideoFrameCallback if available (frame-accurate)
		if ("requestVideoFrameCallback" in element) {
			const video = element as HTMLVideoElement;

			let handle: number;
			const onFrame = () => {
				const timestamp = Moq.Time.Milli.fromSecond(video.currentTime as Moq.Time.Second);
				this.#timestamp.set(timestamp);
				handle = video.requestVideoFrameCallback(onFrame);
			};
			handle = video.requestVideoFrameCallback(onFrame);

			effect.cleanup(() => video.cancelVideoFrameCallback(handle));
		} else {
			// Fallback to timeupdate event
			effect.event(element, "timeupdate", () => {
				const timestamp = Moq.Time.Milli.fromSecond(element.currentTime as Moq.Time.Second);
				this.#timestamp.set(timestamp);
			});
		}
	}

	close(): void {
		this.source.close();
		this.signals.close();
	}
}

async function supported(config: Catalog.VideoConfig): Promise<boolean> {
	return MediaSource.isTypeSupported(`video/mp4; codecs="${config.codec}"`);
}
