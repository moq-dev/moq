import type * as Moq from "@moq/lite";
import type { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../catalog";
import * as Frame from "../frame";
import { PRIORITY } from "../publish/priority";
import * as Mime from "../util/mp4-mime";

// The types in VideoDecoderConfig that cause a hard reload.
type RequiredDecoderConfig = Omit<Catalog.VideoConfig, "codedWidth" | "codedHeight"> &
	Partial<Pick<Catalog.VideoConfig, "codedWidth" | "codedHeight">>;

type BufferStatus = { state: "empty" | "filled" };

type SyncStatus = {
	state: "ready" | "wait";
	bufferDuration?: number;
};

export interface VideoStats {
	frameCount: number;
	timestamp: number;
	bytesReceived: number;
}

/**
 * MSE-based video source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 */
export class SourceMSE {
	#video?: HTMLVideoElement;
	#mediaSource?: MediaSource;
	#videoSourceBuffer?: SourceBuffer;
	#audioSourceBuffer?: SourceBuffer;
	#audioSourceBufferSetup = false;
	#audioInitSegmentAppended = false;

	readonly mediaSource = new Signal<MediaSource | undefined>(undefined);
	readonly videoElement = new Signal<HTMLVideoElement | undefined>(undefined);

	#videoAppendQueue: Uint8Array[] = [];
	#audioAppendQueue: Uint8Array[] = [];
	static readonly MAX_QUEUE_SIZE = 10;

	frame = new Signal<VideoFrame | undefined>(undefined);
	latency: Signal<Time.Milli>;
	display = new Signal<{ width: number; height: number } | undefined>(undefined);
	flip = new Signal<boolean | undefined>(undefined);

	bufferStatus = new Signal<BufferStatus>({ state: "empty" });
	syncStatus = new Signal<SyncStatus>({ state: "ready" });

	#stats = new Signal<VideoStats | undefined>(undefined);

	#signals = new Effect();
	#frameCallbackId?: number;

	constructor(latency: Signal<Time.Milli>) {
		this.latency = latency;
	}

	#isBufferUpdating(): boolean {
		if (!this.#mediaSource) return false;
		const buffers = this.#mediaSource.sourceBuffers;
		for (let i = 0; i < buffers.length; i++) {
			if (buffers[i].updating) {
				return true;
			}
		}
		return false;
	}

	async initializeVideo(config: RequiredDecoderConfig): Promise<void> {
		const mimeType = Mime.buildMp4VideoMimeType(config);
		if (!mimeType) {
			throw new Error(`Unsupported codec for MSE: ${config.codec}`);
		}

		this.#video = document.createElement("video");
		this.#video.style.display = "none";
		this.#video.playsInline = true;
		this.#video.muted = false;
		document.body.appendChild(this.#video);

		this.videoElement.set(this.#video);

		this.#mediaSource = new MediaSource();
		this.mediaSource.set(this.#mediaSource);

		const url = URL.createObjectURL(this.#mediaSource);
		this.#video.src = url;
		await new Promise<void>((resolve, reject) => {
			const timeout = setTimeout(() => {
				reject(new Error("MediaSource sourceopen timeout"));
			}, 5000);

			this.#mediaSource?.addEventListener(
				"sourceopen",
				() => {
					clearTimeout(timeout);
					if (this.#mediaSource) {
						this.mediaSource.set(this.#mediaSource);
					}
					try {
						this.#videoSourceBuffer = this.#mediaSource?.addSourceBuffer(mimeType);
						if (!this.#videoSourceBuffer) {
							reject(new Error("Failed to create video SourceBuffer"));
							return;
						}
						this.#setupVideoSourceBuffer();
						resolve();
					} catch (error) {
						console.error("[MSE] Error creating video SourceBuffer:", error);
						reject(error);
					}
				},
				{ once: true },
			);

			this.#mediaSource?.addEventListener(
				"error",
				(e) => {
					clearTimeout(timeout);
					console.error("[MSE] MediaSource error event:", e);
					reject(new Error(`MediaSource error: ${e}`));
				},
				{ once: true },
			);
		});

		this.#startFrameCapture();
	}

	async initializeAudio(config: Catalog.AudioConfig): Promise<void> {
		if (this.#audioSourceBuffer && this.#audioSourceBufferSetup) {
			return;
		}

		const mimeType = Mime.buildMp4AudioMimeType(config);
		if (!mimeType) {
			throw new Error(`Unsupported codec for MSE: ${config.codec}`);
		}

		// Wait for signal propagation, then get MediaSource
		await new Promise((resolve) => setTimeout(resolve, 10));
		let mediaSource = this.mediaSource.peek();

		if (!mediaSource && this.#mediaSource) {
			mediaSource = this.#mediaSource;
		}

		if (mediaSource && mediaSource.readyState === "open") {
			this.#mediaSource = mediaSource;
		} else {
			await new Promise<void>((resolve, reject) => {
				const maxWait = 5000;
				const startTime = Date.now();
				const checkInterval = 50;

				const timeout = setTimeout(() => {
					const waited = ((Date.now() - startTime) / 1000).toFixed(1);
					reject(
						new Error(
							`MediaSource not ready after ${waited}s (current state: ${mediaSource?.readyState || "not created"})`,
						),
					);
				}, maxWait);

				const checkReady = () => {
					mediaSource = this.mediaSource.peek();

					if (!mediaSource && this.#mediaSource) {
						mediaSource = this.#mediaSource;
					}

					if (mediaSource && mediaSource.readyState === "open") {
						clearTimeout(timeout);
						this.#mediaSource = mediaSource;
						resolve();
						return;
					}

					const elapsed = Date.now() - startTime;

					if (mediaSource && mediaSource.readyState === "closed") {
						if (this.#mediaSource === mediaSource) {
							this.#mediaSource = undefined;
						}
					}

					if (elapsed < maxWait) {
						setTimeout(checkReady, checkInterval);
					} else {
						clearTimeout(timeout);
						const waited = (elapsed / 1000).toFixed(1);
						const finalSignalState = this.mediaSource.peek()?.readyState || "not set";
						const finalPrivateState = this.#mediaSource?.readyState || "not set";
						reject(
							new Error(
								`MediaSource not ready after ${waited}s (signal: ${finalSignalState}, private: ${finalPrivateState})`,
							),
						);
					}
				};

				checkReady();
			});
		}

		mediaSource = this.mediaSource.peek() || this.#mediaSource;
		if (!mediaSource || mediaSource.readyState !== "open") {
			throw new Error(`MediaSource not ready (state: ${mediaSource?.readyState || "not created"})`);
		}

		this.#mediaSource = mediaSource;

		const existingAudioBuffer = this.#resolveExistingAudioSourceBuffer(Array.from(this.#mediaSource.sourceBuffers));
		if (existingAudioBuffer) {
			this.#setAudioSourceBuffer(existingAudioBuffer);
			return;
		}
		if (this.#videoSourceBuffer?.updating) {
			await new Promise<void>((resolve) => {
				if (!this.#videoSourceBuffer) {
					resolve();
					return;
				}
				this.#videoSourceBuffer.addEventListener(
					"updateend",
					() => {
						resolve();
					},
					{ once: true },
				);
			});
		}

		const audioBufferAfterWait = this.#resolveExistingAudioSourceBuffer(
			Array.from(this.#mediaSource.sourceBuffers),
		);
		if (audioBufferAfterWait) {
			this.#setAudioSourceBuffer(audioBufferAfterWait);
			return;
		}

		if (this.#mediaSource.readyState !== "open") {
			throw new Error(
				`MediaSource readyState changed to "${this.#mediaSource.readyState}" before adding audio SourceBuffer`,
			);
		}

		mediaSource = this.mediaSource.peek() || this.#mediaSource;
		if (!mediaSource) {
			throw new Error("MediaSource is not available");
		}

		this.#mediaSource = mediaSource;

		if (this.#videoSourceBuffer?.updating) {
			await new Promise<void>((resolve) => {
				const timeout = setTimeout(() => {
					resolve();
				}, 500);

				if (!this.#videoSourceBuffer) {
					clearTimeout(timeout);
					resolve();
					return;
				}
				this.#videoSourceBuffer.addEventListener(
					"updateend",
					() => {
						clearTimeout(timeout);
						resolve();
					},
					{ once: true },
				);
			});
		}

		const sourceBuffers = Array.from(mediaSource.sourceBuffers);
		if (!MediaSource.isTypeSupported(mimeType)) {
			throw new Error(`Audio MIME type not supported: ${mimeType}`);
		}

		try {
			if (sourceBuffers.length >= 2) {
				console.warn("[MSE] MediaSource already has 2 SourceBuffers, cannot add audio");
				throw new Error("MediaSource already has maximum SourceBuffers");
			}

			this.#audioSourceBuffer = mediaSource.addSourceBuffer(mimeType);
			if (!this.#audioSourceBuffer) {
				throw new Error("Failed to create audio SourceBuffer");
			}
			this.#setAudioSourceBuffer(this.#audioSourceBuffer);
		} catch (error) {
			if (error instanceof DOMException && error.name === "QuotaExceededError") {
				const sourceBuffers = Array.from(mediaSource.sourceBuffers);
				const readyState = mediaSource.readyState;

				if (readyState !== "open") {
					throw new Error(`MediaSource readyState is "${readyState}", cannot add SourceBuffers`);
				}

				const existingAudioBuffer = this.#resolveExistingAudioSourceBuffer(sourceBuffers);
				if (existingAudioBuffer) {
					this.#setAudioSourceBuffer(existingAudioBuffer);
					return;
				}

				if (sourceBuffers.length === 1 && this.#videoSourceBuffer) {
					if (this.#videoSourceBuffer.updating) {
						await new Promise<void>((resolve) => {
							const timeout = setTimeout(() => resolve(), 200);
							if (!this.#videoSourceBuffer) {
								clearTimeout(timeout);
								resolve();
								return;
							}
							this.#videoSourceBuffer.addEventListener(
								"updateend",
								() => {
									clearTimeout(timeout);
									resolve();
								},
								{ once: true },
							);
						});
					} else {
						await new Promise((resolve) => setTimeout(resolve, 10));
					}

					const currentSourceBuffers = Array.from(mediaSource.sourceBuffers);
					if (currentSourceBuffers.length >= 2) {
						const retryAudioBuffer = this.#resolveExistingAudioSourceBuffer(currentSourceBuffers);
						if (retryAudioBuffer) {
							this.#setAudioSourceBuffer(retryAudioBuffer);
							return;
						}
					}

					try {
						if (mediaSource.readyState !== "open") {
							throw new Error(`MediaSource readyState is "${mediaSource.readyState}"`);
						}
						this.#audioSourceBuffer = mediaSource.addSourceBuffer(mimeType);
						if (!this.#audioSourceBuffer) {
							throw new Error("Failed to create audio SourceBuffer");
						}
						this.#setAudioSourceBuffer(this.#audioSourceBuffer);
						return;
					} catch (retryError) {
						console.warn("[MSE] Retry failed, allowing video-only playback", retryError);
						return;
					}
				}

				console.warn("[MSE] QuotaExceededError but couldn't find audio SourceBuffer in MediaSource", {
					sourceBufferCount: sourceBuffers.length,
					readyState: mediaSource.readyState,
					hasVideoSourceBuffer: !!this.#videoSourceBuffer,
					hasAudioSourceBuffer: !!this.#audioSourceBuffer,
				});
			}
			console.error("[MSE] Error adding audio SourceBuffer:", error);
			throw error;
		}
	}

	#setupVideoSourceBuffer(): void {
		if (!this.#videoSourceBuffer) return;

		const SEEK_HYSTERESIS = 0.1;
		this.#videoSourceBuffer.addEventListener("updateend", () => {
			const video = this.#video;
			const sourceBuffer = this.#videoSourceBuffer;
			if (video && sourceBuffer && sourceBuffer.buffered.length > 0) {
				const buffered = sourceBuffer.buffered;
				const start = buffered.start(0);
				const end = buffered.end(0);

				if (
					video.currentTime + SEEK_HYSTERESIS < start ||
					video.currentTime >= end - SEEK_HYSTERESIS ||
					Number.isNaN(video.currentTime)
				) {
					video.currentTime = start;
				}

				if (video.paused && video.readyState >= HTMLMediaElement.HAVE_METADATA) {
					video.play().catch((err) => {
						console.warn("[MSE] Autoplay blocked:", err);
					});
				}
			}

			this.#processVideoQueue();
		});

		this.#videoSourceBuffer.addEventListener("error", (e) => {
			console.error("[MSE] Video SourceBuffer error:", e);
		});
	}

	#setupAudioSourceBuffer(): void {
		if (!this.#audioSourceBuffer || this.#audioSourceBufferSetup) return;

		this.#audioSourceBuffer.addEventListener("updateend", () => {
			this.#processAudioQueue();
		});

		this.#audioSourceBuffer.addEventListener("error", (e) => {
			console.error("[MSE] Audio SourceBuffer error:", e);
		});

		this.#audioSourceBufferSetup = true;
	}

	#startFrameCapture(): void {
		if (!this.#video) return;

		const captureFrame = () => {
			if (!this.#video) return;

			try {
				const frame = new VideoFrame(this.#video, {
					timestamp: this.#video.currentTime * 1_000_000,
				});

				this.#stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + 1,
					timestamp: frame.timestamp,
					bytesReceived: current?.bytesReceived ?? 0,
				}));

				this.frame.update((prev) => {
					prev?.close();
					return frame;
				});

				if (this.#video.videoWidth && this.#video.videoHeight) {
					this.display.set({
						width: this.#video.videoWidth,
						height: this.#video.videoHeight,
					});
				}

				if (this.#video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
					this.bufferStatus.set({ state: "filled" });
					if (this.#video.paused && this.#video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
						this.#video.play().catch(() => {
							// Ignore autoplay errors
						});
					}
				}
			} catch (error) {
				console.error("Error capturing frame:", error);
			}

			if (this.#video.requestVideoFrameCallback) {
				this.#frameCallbackId = this.#video.requestVideoFrameCallback(captureFrame);
			} else {
				this.#frameCallbackId = requestAnimationFrame(captureFrame) as unknown as number;
			}
		};

		if (this.#video.requestVideoFrameCallback) {
			this.#frameCallbackId = this.#video.requestVideoFrameCallback(captureFrame);
		} else {
			this.#frameCallbackId = requestAnimationFrame(captureFrame) as unknown as number;
		}
	}

	async appendVideoFragment(fragment: Uint8Array): Promise<void> {
		if (!this.#videoSourceBuffer || !this.#mediaSource) {
			throw new Error("Video SourceBuffer not initialized");
		}

		if (this.#videoAppendQueue.length >= SourceMSE.MAX_QUEUE_SIZE) {
			const discarded = this.#videoAppendQueue.shift();
			console.warn(
				`[MSE] Video queue full (${SourceMSE.MAX_QUEUE_SIZE}), discarding oldest fragment (${discarded?.byteLength ?? 0} bytes)`,
			);
		}

		const copy = new Uint8Array(fragment);
		this.#videoAppendQueue.push(copy);
		this.#processVideoQueue();
	}

	async appendAudioFragment(fragment: Uint8Array): Promise<void> {
		if (!this.#audioSourceBuffer || !this.#mediaSource) {
			return;
		}

		if (this.#mediaSource.readyState === "closed") {
			return;
		}

		if (this.#audioAppendQueue.length >= SourceMSE.MAX_QUEUE_SIZE) {
			const discarded = this.#audioAppendQueue.shift();
			console.warn(
				`[MSE] Audio queue full (${SourceMSE.MAX_QUEUE_SIZE}), discarding oldest fragment (${discarded?.byteLength ?? 0} bytes)`,
			);
		}

		const copy = new Uint8Array(fragment);
		this.#audioAppendQueue.push(copy);
		this.#processAudioQueue();
	}

	/**
	 * Decode base64 init segment string to Uint8Array.
	 */
	#decodeInitSegment(base64: string): Uint8Array {
		const binaryString = atob(base64);
		const initSegment = new Uint8Array(binaryString.length);
		for (let i = 0; i < binaryString.length; i++) {
			initSegment[i] = binaryString.charCodeAt(i);
		}
		return initSegment;
	}

	#setAudioSourceBuffer(buffer: SourceBuffer): void {
		this.#audioSourceBuffer = buffer;
		if (!this.#audioSourceBufferSetup) {
			this.#setupAudioSourceBuffer();
		}
	}

	#resolveExistingAudioSourceBuffer(sourceBuffers: SourceBuffer[]): SourceBuffer | undefined {
		if (this.#audioSourceBuffer && sourceBuffers.includes(this.#audioSourceBuffer)) {
			return this.#audioSourceBuffer;
		}

		if (sourceBuffers.length < 2) {
			return undefined;
		}

		if (this.#videoSourceBuffer) {
			const otherBuffer = sourceBuffers.find((sb) => sb !== this.#videoSourceBuffer);
			if (otherBuffer) {
				return otherBuffer;
			}
		} else {
			return sourceBuffers[1];
		}

		throw new Error("MediaSource already has maximum SourceBuffers and cannot identify audio SourceBuffer");
	}

	/**
	 * Append an init segment to a SourceBuffer and wait for completion.
	 * Handles both synchronous errors and async errors via events.
	 */
	async #appendInitSegment(
		sourceBuffer: SourceBuffer,
		initSegment: Uint8Array,
		trackType: "video" | "audio",
	): Promise<void> {
		await new Promise<void>((resolve, reject) => {
			const onUpdateEnd = () => {
				resolve();
			};

			const onError = (e: Event) => {
				const error = e as ErrorEvent;
				console.error(`[MSE] ${trackType} SourceBuffer error appending init segment:`, error);
				reject(new Error(`${trackType} SourceBuffer error: ${error.message || "unknown error"}`));
			};

			sourceBuffer.addEventListener("updateend", onUpdateEnd, { once: true });
			sourceBuffer.addEventListener("error", onError, { once: true });

			try {
				sourceBuffer.appendBuffer(initSegment as BufferSource);
			} catch (error) {
				sourceBuffer.removeEventListener("updateend", onUpdateEnd);
				sourceBuffer.removeEventListener("error", onError);
				console.error(`[MSE] Error calling appendBuffer on ${trackType} init segment:`, error);
				reject(error);
			}
		});
	}

	#processVideoQueue(): void {
		if (!this.#videoSourceBuffer || this.#videoSourceBuffer.updating || this.#videoAppendQueue.length === 0) {
			return;
		}

		if (this.#mediaSource?.readyState !== "open") {
			return;
		}

		if (this.#isBufferUpdating()) {
			return;
		}

		const fragment = this.#videoAppendQueue.shift();
		if (!fragment) return;

		try {
			this.#videoSourceBuffer.appendBuffer(fragment as BufferSource);
			this.#stats.update((current) => {
				const newCount = (current?.frameCount ?? 0) + 1;
				return {
					frameCount: newCount,
					timestamp: current?.timestamp ?? 0,
					bytesReceived: (current?.bytesReceived ?? 0) + fragment.byteLength,
				};
			});
		} catch (error) {
			if (error instanceof DOMException && error.name === "QuotaExceededError") {
				console.warn("[MSE] QuotaExceededError - browser will manage buffer automatically");
				this.#videoAppendQueue.unshift(fragment);
			} else {
				console.error("[MSE] Error appending video fragment:", error);
			}
		}
	}

	#processAudioQueue(): void {
		if (!this.#audioSourceBuffer || this.#audioSourceBuffer.updating || this.#audioAppendQueue.length === 0) {
			return;
		}

		if (this.#mediaSource?.readyState !== "open") {
			return;
		}

		if (this.#isBufferUpdating()) {
			return;
		}

		const fragment = this.#audioAppendQueue.shift();
		if (!fragment) return;

		try {
			this.#audioSourceBuffer.appendBuffer(fragment as BufferSource);
		} catch (error) {
			if (error instanceof DOMException && error.name === "QuotaExceededError") {
				console.warn("[MSE] QuotaExceededError for audio - browser will manage buffer automatically");
				this.#audioAppendQueue.unshift(fragment);
			} else {
				console.error("[MSE] Error appending audio fragment:", error);
			}
		}
	}

	async appendFragment(fragment: Uint8Array): Promise<void> {
		return this.appendVideoFragment(fragment);
	}

	async runTrack(
		effect: Effect,
		broadcast: Moq.Broadcast,
		name: string,
		config: RequiredDecoderConfig,
	): Promise<void> {
		await this.initializeVideo(config);

		// Wait for audio SourceBuffer initialization to avoid Chrome quota race
		for (let i = 0; i < 10; i++) {
			if (this.#audioSourceBuffer || (this.#mediaSource && this.#mediaSource.sourceBuffers.length >= 2)) {
				break;
			}
			await new Promise((resolve) => setTimeout(resolve, 100));
		}

		const sub = broadcast.subscribe(name, PRIORITY.video);
		console.log(`[MSE] Subscribing to video track: ${name}`);
		effect.cleanup(() => sub.close());

		const consumer = new Frame.Consumer(sub, {
			latency: this.latency,
			container: "cmaf",
		});
		effect.cleanup(() => consumer.close());

		if (!config.initSegment) {
			throw new Error("Init segment is required in catalog for CMAF playback");
		}

		if (!this.#videoSourceBuffer) {
			throw new Error("Video SourceBuffer not available");
		}

		const initSegment = this.#decodeInitSegment(config.initSegment);
		await this.#appendInitSegment(this.#videoSourceBuffer, initSegment, "video");
		effect.spawn(async () => {
			for (;;) {
				const frame = await Promise.race([consumer.decode(), effect.cancel]);
				if (!frame) {
					break;
				}

				await this.appendVideoFragment(frame.data);
			}
		});
	}

	async runAudioTrack(
		effect: Effect,
		broadcast: Moq.Broadcast,
		name: string,
		config: Catalog.AudioConfig,
		catalog: Catalog.Audio,
		enabled?: Getter<boolean>,
	): Promise<void> {
		if (!this.#audioSourceBuffer) {
			return;
		}

		const isEnabled = enabled ? effect.get(enabled) : true;
		if (!isEnabled) {
			return;
		}

		if (!config.initSegment) {
			throw new Error("Init segment is required in catalog for CMAF playback");
		}

		if (!this.#audioInitSegmentAppended) {
			const initSegment = this.#decodeInitSegment(config.initSegment);
			await this.#appendInitSegment(this.#audioSourceBuffer, initSegment, "audio");
			this.#audioInitSegmentAppended = true;
		}

		const sub = broadcast.subscribe(name, catalog.priority);
		console.log(`[MSE] Subscribing to audio track: ${name}`);
		effect.cleanup(() => sub.close());

		const consumer = new Frame.Consumer(sub, {
			latency: this.latency,
			container: "cmaf",
		});
		effect.cleanup(() => consumer.close());

		effect.spawn(async () => {
			for (;;) {
				const frame = await Promise.race([consumer.decode(), effect.cancel]);
				if (!frame) {
					break;
				}

				if (this.#mediaSource?.readyState === "closed") {
					break;
				}

				if (this.#mediaSource?.readyState === "open") {
					await this.appendAudioFragment(frame.data);
				}
			}
		});
	}

	close(): void {
		this.#videoAppendQueue = [];
		this.#audioAppendQueue = [];
		this.#audioSourceBufferSetup = false;
		this.#audioInitSegmentAppended = false;

		const audioSourceBuffer = this.#audioSourceBuffer;
		const videoSourceBuffer = this.#videoSourceBuffer;
		const mediaSource = this.#mediaSource;

		this.#audioSourceBuffer = undefined;

		this.mediaSource.set(undefined);

		if (this.#frameCallbackId !== undefined) {
			if (this.#video?.requestVideoFrameCallback) {
				this.#video.cancelVideoFrameCallback(this.#frameCallbackId);
			} else {
				cancelAnimationFrame(this.#frameCallbackId);
			}
		}

		this.frame.update((prev) => {
			prev?.close();
			return undefined;
		});

		if (videoSourceBuffer && mediaSource) {
			try {
				if (videoSourceBuffer.updating) {
					videoSourceBuffer.abort();
				}
			} catch (error) {
				console.error("Error closing video SourceBuffer:", error);
			}
		}

		if (audioSourceBuffer && mediaSource) {
			try {
				if (audioSourceBuffer.updating) {
					audioSourceBuffer.abort();
				}
			} catch (error) {
				console.error("Error closing audio SourceBuffer:", error);
			}
		}

		if (this.#mediaSource) {
			try {
				if (this.#mediaSource.readyState === "open") {
					this.#mediaSource.endOfStream();
				}
				URL.revokeObjectURL(this.#video?.src || "");
			} catch (error) {
				console.error("Error closing MediaSource:", error);
			}
		}

		if (this.#video) {
			this.#video.pause();
			this.#video.src = "";
			this.#video.remove();
		}

		this.#signals.close();
	}

	get stats() {
		return this.#stats;
	}
}
