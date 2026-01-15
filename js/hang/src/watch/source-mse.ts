import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../catalog";
import * as Frame from "../frame";
import { PRIORITY } from "../publish/priority";
import type * as Time from "../time";
import * as Mime from "../util/mime";

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
	#audioSourceBufferSetup = false; // Track if audio SourceBuffer has been set up

	readonly mediaSource = new Signal<MediaSource | undefined>(undefined);

	// Expose video element for audio control (audio plays through video element)
	readonly videoElement = new Signal<HTMLVideoElement | undefined>(undefined);

	// Queue of fragments waiting to be added for video
	#videoAppendQueue: Uint8Array[] = [];
	// Queue of fragments waiting to be added for audio
	#audioAppendQueue: Uint8Array[] = [];
	static readonly MAX_QUEUE_SIZE = 10; // Maximum fragments in queue

	// Expose the current frame to render as a signal
	frame = new Signal<VideoFrame | undefined>(undefined);

	// The target latency in milliseconds.
	latency: Signal<Time.Milli>;

	// The display size of the video in pixels.
	display = new Signal<{ width: number; height: number } | undefined>(undefined);

	// Whether to flip the video horizontally.
	flip = new Signal<boolean | undefined>(undefined);

	bufferStatus = new Signal<BufferStatus>({ state: "empty" });
	syncStatus = new Signal<SyncStatus>({ state: "ready" });

	#stats = new Signal<VideoStats | undefined>(undefined);

	#signals = new Effect();
	#frameCallbackId?: number;

	constructor(latency: Signal<Time.Milli>) {
		this.latency = latency;
	}

	/**
	 * Check if any SourceBuffer is updating
	 */
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
		const mimeType = Mime.buildVideoMimeType(config);
		if (!mimeType) {
			throw new Error(`Unsupported codec for MSE: ${config.codec}`);
		}

		console.log("[MSE] Initializing video, MIME type:", mimeType);

		// Create video element
		this.#video = document.createElement("video");
		this.#video.style.display = "none";
		this.#video.playsInline = true;
		this.#video.muted = false; // Don't mute - audio plays through video element
		document.body.appendChild(this.#video);

		// Expose video element
		this.videoElement.set(this.#video);

		// Create MediaSource
		this.#mediaSource = new MediaSource();
		this.mediaSource.set(this.#mediaSource);
		console.log("[MSE] Video initialization: MediaSource signal set, state:", this.#mediaSource.readyState);

		// Attach MediaSource to video element
		const url = URL.createObjectURL(this.#mediaSource);
		this.#video.src = url;
		console.log("[MSE] MediaSource created and attached to video element");

		// Wait for sourceopen event
		await new Promise<void>((resolve, reject) => {
			const timeout = setTimeout(() => {
				reject(new Error("MediaSource sourceopen timeout"));
			}, 5000);

			this.#mediaSource?.addEventListener(
				"sourceopen",
				() => {
					clearTimeout(timeout);
					console.log("[MSE] MediaSource sourceopen event fired");
					// Update signal to ensure audio sees the open MediaSource
					if (this.#mediaSource) {
						this.mediaSource.set(this.#mediaSource);
					}
					try {
						this.#videoSourceBuffer = this.#mediaSource?.addSourceBuffer(mimeType);
						if (!this.#videoSourceBuffer) {
							reject(new Error("Failed to create video SourceBuffer"));
							return;
						}
						console.log("[MSE] Video SourceBuffer created successfully");
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

		console.log("[MSE] Video initialization complete, starting frame capture");
		this.#startFrameCapture();
	}

	async initializeAudio(config: Catalog.AudioConfig): Promise<void> {
		// Early return if already initialized
		if (this.#audioSourceBuffer && this.#audioSourceBufferSetup) {
			console.log("[MSE] Audio SourceBuffer already initialized, skipping");
			return;
		}

		const mimeType = Mime.buildAudioMimeType(config);
		if (!mimeType) {
			throw new Error(`Unsupported codec for MSE: ${config.codec}`);
		}

		console.log("[MSE] Initializing audio, MIME type:", mimeType);

		// Get MediaSource from signal (most up-to-date)
		// Use a small delay to ensure signal updates have propagated
		await new Promise((resolve) => setTimeout(resolve, 10));
		let mediaSource = this.mediaSource.peek();
		console.log(
			"[MSE] Audio initialization: MediaSource from signal:",
			mediaSource ? `readyState=${mediaSource.readyState}` : "not set",
		);

		// Also check private field as fallback
		if (!mediaSource && this.#mediaSource) {
			console.log(
				"[MSE] Audio initialization: Using private MediaSource field, state:",
				this.#mediaSource.readyState,
			);
			mediaSource = this.#mediaSource;
		}

		// Quick check: if MediaSource is ready, proceed immediately
		if (mediaSource && mediaSource.readyState === "open") {
			console.log("[MSE] Audio initialization: MediaSource is already open, proceeding");
			this.#mediaSource = mediaSource;
		} else {
			console.log("[MSE] Audio initialization: MediaSource not ready, waiting...");
			// Wait for MediaSource to be created and open (video initialization is async)
			// Use a longer timeout to allow video to restart properly
			await new Promise<void>((resolve, reject) => {
				const maxWait = 5000; // 5 seconds max wait
				const startTime = Date.now();
				const checkInterval = 50; // Check every 50ms for responsiveness

				const timeout = setTimeout(() => {
					const waited = ((Date.now() - startTime) / 1000).toFixed(1);
					reject(
						new Error(
							`MediaSource not ready after ${waited}s (current state: ${mediaSource?.readyState || "not created"})`,
						),
					);
				}, maxWait);

				const checkReady = () => {
					// Get latest MediaSource from signal (always get fresh value)
					const signalValue = this.mediaSource.peek();
					mediaSource = signalValue;

					// Also check private field if signal is not set
					if (!mediaSource && this.#mediaSource) {
						mediaSource = this.#mediaSource;
					}

					// Check if MediaSource exists and is open
					if (mediaSource && mediaSource.readyState === "open") {
						clearTimeout(timeout);
						this.#mediaSource = mediaSource;
						const elapsed = ((Date.now() - startTime) / 1000).toFixed(2);
						console.log(`[MSE] Audio initialization: MediaSource is ready (waited ${elapsed}s)`);
						resolve();
						return;
					}

					// Log progress for debugging (every 0.5 seconds)
					const elapsed = Date.now() - startTime;
					if (elapsed % 500 < checkInterval) {
						const signalState = this.mediaSource.peek()?.readyState || "not set";
						const privateState = this.#mediaSource?.readyState || "not set";
						console.log(
							`[MSE] Audio initialization: Waiting for MediaSource (${(elapsed / 1000).toFixed(1)}s, signal: ${signalState}, private: ${privateState})`,
						);
					}

					// If MediaSource exists but is closed, it's from an old instance - wait for new one
					if (mediaSource && mediaSource.readyState === "closed") {
						// Reset private field
						if (this.#mediaSource === mediaSource) {
							this.#mediaSource = undefined;
						}
					}

					// Continue checking if we haven't exceeded max wait time
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

		// Final check - ensure we have a MediaSource
		mediaSource = this.mediaSource.peek() || this.#mediaSource;
		if (!mediaSource || mediaSource.readyState !== "open") {
			throw new Error(`MediaSource not ready (state: ${mediaSource?.readyState || "not created"})`);
		}

		// Update private field
		this.#mediaSource = mediaSource;

		// Check if MediaSource already has an audio SourceBuffer
		// (could be added by a previous call to initializeAudio)
		if (this.#mediaSource.sourceBuffers.length >= 2) {
			const sourceBuffers = Array.from(this.#mediaSource.sourceBuffers);

			// If we already have an audio SourceBuffer set, use it
			if (this.#audioSourceBuffer && sourceBuffers.includes(this.#audioSourceBuffer)) {
				return; // Already have it
			}

			// If we have exactly 2 SourceBuffers and one is video, the other must be audio
			if (sourceBuffers.length === 2 && this.#videoSourceBuffer) {
				const otherBuffer = sourceBuffers.find((sb) => sb !== this.#videoSourceBuffer);
				if (otherBuffer) {
					// This must be the audio SourceBuffer
					this.#audioSourceBuffer = otherBuffer;
					if (!this.#audioSourceBufferSetup) {
						this.#setupAudioSourceBuffer();
					}
					return;
				}
			}

			// Fallback: If we have 2 SourceBuffers but don't know which is video
			// Assume the second one is audio (video is usually added first)
			if (sourceBuffers.length === 2 && !this.#videoSourceBuffer) {
				console.log(
					"[MSE] Video SourceBuffer not set yet, using fallback: assuming second SourceBuffer is audio",
				);
				this.#audioSourceBuffer = sourceBuffers[1];
				if (!this.#audioSourceBufferSetup) {
					this.#setupAudioSourceBuffer();
				}
				return;
			}

			// MediaSource has 2 SourceBuffers but we can't identify which is audio
			// This shouldn't happen, but handle gracefully
			throw new Error("MediaSource already has maximum SourceBuffers and cannot identify audio SourceBuffer");
		}

		// Double-check audio SourceBuffer wasn't set while we were waiting
		if (this.#audioSourceBuffer) {
			return;
		}

		// Wait for video SourceBuffer to finish if updating
		if (this.#videoSourceBuffer?.updating) {
			console.log("[MSE] Waiting for video SourceBuffer to finish updating before adding audio");
			await new Promise<void>((resolve) => {
				if (!this.#videoSourceBuffer) {
					resolve();
					return;
				}
				this.#videoSourceBuffer.addEventListener(
					"updateend",
					() => {
						console.log("[MSE] Video SourceBuffer finished updating");
						resolve();
					},
					{ once: true },
				);
			});
		}

		// Final check before adding
		if (this.#audioSourceBuffer) {
			return;
		}

		// Check again if MediaSource now has 2 SourceBuffers (race condition)
		if (this.#mediaSource.sourceBuffers.length >= 2) {
			const sourceBuffers = Array.from(this.#mediaSource.sourceBuffers);

			// If we already have audio SourceBuffer set, use it
			if (this.#audioSourceBuffer && sourceBuffers.includes(this.#audioSourceBuffer)) {
				return;
			}

			// If we have exactly 2 and one is video, use the other
			if (sourceBuffers.length === 2 && this.#videoSourceBuffer) {
				const otherBuffer = sourceBuffers.find((sb) => sb !== this.#videoSourceBuffer);
				if (otherBuffer) {
					this.#audioSourceBuffer = otherBuffer;
					if (!this.#audioSourceBufferSetup) {
						this.#setupAudioSourceBuffer();
					}
					return;
				}
			}

			// Fallback: If we have 2 SourceBuffers but don't know which is video
			if (sourceBuffers.length === 2 && !this.#videoSourceBuffer) {
				console.log("[MSE] Race condition: Video SourceBuffer not set yet, using fallback");
				this.#audioSourceBuffer = sourceBuffers[1]; // Assume second is audio
				if (!this.#audioSourceBufferSetup) {
					this.#setupAudioSourceBuffer();
				}
				return;
			}

			throw new Error("MediaSource already has maximum SourceBuffers and cannot identify audio SourceBuffer");
		}

		// Final check before adding - verify MediaSource is still open
		if (this.#mediaSource.readyState !== "open") {
			throw new Error(
				`MediaSource readyState changed to "${this.#mediaSource.readyState}" before adding audio SourceBuffer`,
			);
		}

		// Ensure we're using the MediaSource from signal (most up-to-date)
		mediaSource = this.mediaSource.peek() || this.#mediaSource;
		if (!mediaSource) {
			throw new Error("MediaSource is not available");
		}

		// Update private field to match signal
		this.#mediaSource = mediaSource;

		// Wait for video SourceBuffer to finish updating before adding audio SourceBuffer
		// Only wait if it's actually updating (should be rare)
		if (this.#videoSourceBuffer?.updating) {
			console.log("[MSE] Video SourceBuffer is updating, waiting briefly");
			await new Promise<void>((resolve) => {
				const timeout = setTimeout(() => {
					// Don't wait too long - proceed anyway
					console.log("[MSE] Video SourceBuffer update timeout, proceeding");
					resolve();
				}, 500); // Only wait 500ms max

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

		// Log state before adding
		const sourceBuffers = Array.from(mediaSource.sourceBuffers);
		console.log("[MSE] About to add audio SourceBuffer", {
			audioMimeType: mimeType,
			sourceBufferCount: sourceBuffers.length,
			videoSourceBufferUpdating: this.#videoSourceBuffer?.updating,
			readyState: mediaSource.readyState,
			isAudioMimeTypeSupported: MediaSource.isTypeSupported(mimeType),
		});

		// Double-check MIME type is supported
		if (!MediaSource.isTypeSupported(mimeType)) {
			throw new Error(`Audio MIME type not supported: ${mimeType}`);
		}

		// Some browsers have quirks - try to add SourceBuffer and handle errors gracefully
		try {
			// Check if we can actually add another SourceBuffer
			// Some browsers might report 1 SourceBuffer but actually be at limit
			if (sourceBuffers.length >= 2) {
				console.warn("[MSE] MediaSource already has 2 SourceBuffers, cannot add audio");
				throw new Error("MediaSource already has maximum SourceBuffers");
			}

			this.#audioSourceBuffer = mediaSource.addSourceBuffer(mimeType);
			if (!this.#audioSourceBuffer) {
				throw new Error("Failed to create audio SourceBuffer");
			}
			console.log("[MSE] Audio SourceBuffer created successfully");
			this.#setupAudioSourceBuffer();
		} catch (error) {
			// If QuotaExceededError, check if another call added the audio SourceBuffer
			if (error instanceof DOMException && error.name === "QuotaExceededError") {
				const sourceBuffers = Array.from(mediaSource.sourceBuffers);
				const readyState = mediaSource.readyState;
				console.log("[MSE] QuotaExceededError - MediaSource has", sourceBuffers.length, "SourceBuffers", {
					readyState,
					videoSourceBufferSet: !!this.#videoSourceBuffer,
					audioSourceBufferSet: !!this.#audioSourceBuffer,
				});

				// If MediaSource is not open, that's the problem
				if (readyState !== "open") {
					throw new Error(`MediaSource readyState is "${readyState}", cannot add SourceBuffers`);
				}

				// If we already have audio SourceBuffer set, use it
				if (this.#audioSourceBuffer && sourceBuffers.includes(this.#audioSourceBuffer)) {
					console.log("[MSE] Found existing audio SourceBuffer reference");
					return; // Success - silently return
				}

				// If we have exactly 2 SourceBuffers and one is video, the other must be audio
				if (sourceBuffers.length === 2 && this.#videoSourceBuffer) {
					const otherBuffer = sourceBuffers.find((sb) => sb !== this.#videoSourceBuffer);
					if (otherBuffer) {
						console.log("[MSE] Found audio SourceBuffer by exclusion (other than video)");
						this.#audioSourceBuffer = otherBuffer;
						if (!this.#audioSourceBufferSetup) {
							this.#setupAudioSourceBuffer();
						}
						return; // Success - silently return
					}
				}

				// If we have 2 SourceBuffers but don't know which is video, try to identify by checking if one is already set
				// This handles the case where video SourceBuffer isn't set yet
				if (sourceBuffers.length === 2) {
					// If we don't have video SourceBuffer set, we can't reliably identify which is audio
					// But if one of them was added by a previous call to initializeAudio, we should use it
					// For now, if we have 2 SourceBuffers and can't identify, assume the first non-video one is audio
					// This is a fallback - ideally video should initialize first
					const nonVideoBuffer = this.#videoSourceBuffer
						? sourceBuffers.find((sb) => sb !== this.#videoSourceBuffer)
						: sourceBuffers[1]; // If video not set, assume second one is audio (video is usually first)

					if (nonVideoBuffer) {
						console.log("[MSE] Using fallback: assuming non-video SourceBuffer is audio");
						this.#audioSourceBuffer = nonVideoBuffer;
						if (!this.#audioSourceBufferSetup) {
							this.#setupAudioSourceBuffer();
						}
						return; // Success - silently return
					}
				}

				// If we have only 1 SourceBuffer and get QuotaExceededError, this is unusual
				// It might mean the video SourceBuffer is updating or MediaSource is in a transitional state
				// Wait briefly and retry once
				if (sourceBuffers.length === 1 && this.#videoSourceBuffer) {
					console.log("[MSE] QuotaExceededError with only 1 SourceBuffer - retrying once", {
						readyState: mediaSource.readyState,
						videoSourceBufferUpdating: this.#videoSourceBuffer.updating,
					});

					// Wait for video SourceBuffer to finish if it's updating (with timeout)
					if (this.#videoSourceBuffer.updating) {
						await new Promise<void>((resolve) => {
							const timeout = setTimeout(() => resolve(), 200); // Max 200ms wait
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
						// Brief wait for MediaSource to stabilize
						await new Promise((resolve) => setTimeout(resolve, 10));
					}

					// Quick retry - check if another call added it first
					const currentSourceBuffers = Array.from(mediaSource.sourceBuffers);
					if (currentSourceBuffers.length >= 2) {
						const otherBuffer = currentSourceBuffers.find((sb) => sb !== this.#videoSourceBuffer);
						if (otherBuffer) {
							console.log("[MSE] Found audio SourceBuffer after retry");
							this.#audioSourceBuffer = otherBuffer;
							if (!this.#audioSourceBufferSetup) {
								this.#setupAudioSourceBuffer();
							}
							return;
						}
					}

					// Try adding again
					try {
						if (mediaSource.readyState !== "open") {
							throw new Error(`MediaSource readyState is "${mediaSource.readyState}"`);
						}
						this.#audioSourceBuffer = mediaSource.addSourceBuffer(mimeType);
						if (!this.#audioSourceBuffer) {
							throw new Error("Failed to create audio SourceBuffer");
						}
						console.log("[MSE] Audio SourceBuffer created successfully after retry");
						this.#setupAudioSourceBuffer();
						return; // Success
					} catch (retryError) {
						// If retry also fails, allow video-only playback (don't delay further)
						console.warn("[MSE] Retry failed, allowing video-only playback", retryError);
						return;
					}
				}

				// If we still can't find it, log details and rethrow
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

		const SEEK_HYSTERESIS = 0.1; // seconds to avoid re-seek loops on tiny drift
		this.#videoSourceBuffer.addEventListener("updateend", () => {
			// Check if we have buffered data and try to play
			const video = this.#video;
			const sourceBuffer = this.#videoSourceBuffer;
			if (video && sourceBuffer && sourceBuffer.buffered.length > 0) {
				const buffered = sourceBuffer.buffered;
				const start = buffered.start(0);
				const end = buffered.end(0);

				// Seek to start of buffered range if needed
				if (
					video.currentTime + SEEK_HYSTERESIS < start ||
					video.currentTime >= end - SEEK_HYSTERESIS ||
					Number.isNaN(video.currentTime)
				) {
					console.log(`[MSE] Seeking video to buffered range start: ${start.toFixed(2)}`);
					video.currentTime = start;
				}

				// Try to play if paused
				if (video.paused && video.readyState >= HTMLMediaElement.HAVE_METADATA) {
					console.log("[MSE] Attempting to play video after SourceBuffer updateend");
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

		let captureCount = 0;
		const captureFrame = () => {
			if (!this.#video) return;

			try {
				const frame = new VideoFrame(this.#video, {
					timestamp: this.#video.currentTime * 1_000_000, // Convert to microseconds
				});

				captureCount++;
				if (captureCount === 1 || captureCount % 30 === 0) {
					console.log(
						`[MSE] Captured frame ${captureCount}, currentTime: ${this.#video.currentTime.toFixed(2)}, readyState: ${this.#video.readyState}, paused: ${this.#video.paused}, buffered: ${this.#video.buffered.length > 0 ? `${this.#video.buffered.start(0).toFixed(2)}-${this.#video.buffered.end(0).toFixed(2)}` : "none"}`,
					);
				}

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
					// Try to play if paused and we have data
					if (this.#video.paused && this.#video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
						this.#video.play().catch((err) => {
							if (captureCount <= 5) {
								console.log("[MSE] Attempted to play video, result:", err);
							}
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
		// If audio SourceBuffer doesn't exist, silently return (video-only playback)
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
	 * Extracts a track-specific init segment from a full init segment.
	 * MSE requires track-specific init segments for each SourceBuffer.
	 */
	#extractTrackInitSegment(fullInitSegment: Uint8Array, trackType: "video" | "audio"): Uint8Array {
		let offset = 0;
		let ftypAtom: Uint8Array | null = null;
		let moovOffset = 0;
		let moovSize = 0;

		// Find ftyp and moov atoms
		while (offset + 8 <= fullInitSegment.length) {
			const size =
				(fullInitSegment[offset] << 24) |
				(fullInitSegment[offset + 1] << 16) |
				(fullInitSegment[offset + 2] << 8) |
				fullInitSegment[offset + 3];
			const type = String.fromCharCode(
				fullInitSegment[offset + 4],
				fullInitSegment[offset + 5],
				fullInitSegment[offset + 6],
				fullInitSegment[offset + 7],
			);

			if (type === "ftyp") {
				ftypAtom = fullInitSegment.slice(offset, offset + size);
				offset += size;
			} else if (type === "moov") {
				moovOffset = offset;
				moovSize = size;
				break;
			} else {
				if (size < 8 || size === 0) break;
				offset += size;
			}
		}

		if (moovSize === 0) {
			throw new Error("moov atom not found in init segment");
		}

		// Parse moov atom to find the relevant track
		const moovAtom = fullInitSegment.slice(moovOffset, moovOffset + moovSize);
		const targetHandler = trackType === "video" ? "vide" : "soun";

		// Count tracks in moov
		let moov_track_count = 0;
		let moov_offset_temp = 8;
		while (moov_offset_temp + 8 <= moovAtom.length) {
			const size =
				(moovAtom[moov_offset_temp] << 24) |
				(moovAtom[moov_offset_temp + 1] << 16) |
				(moovAtom[moov_offset_temp + 2] << 8) |
				moovAtom[moov_offset_temp + 3];
			const type = String.fromCharCode(
				moovAtom[moov_offset_temp + 4],
				moovAtom[moov_offset_temp + 5],
				moovAtom[moov_offset_temp + 6],
				moovAtom[moov_offset_temp + 7],
			);
			if (type === "trak") {
				moov_track_count++;
			}
			if (size < 8 || size === 0) break;
			moov_offset_temp += size;
		}

		// If only one track, use directly
		if (moov_track_count === 1) {
			return fullInitSegment;
		}

		// Multiple tracks - need to extract
		const trakAtom = this.#findTrackInMoov(moovAtom, targetHandler);
		if (!trakAtom) {
			// Try alternative handler types
			const alternatives = trackType === "video" ? ["vid ", "vide", "avc1"] : ["soun", "mp4a"];
			for (const alt of alternatives) {
				const altTrak = this.#findTrackInMoov(moovAtom, alt);
				if (altTrak) {
					return this.#extractTrackInitSegmentWithHandler(fullInitSegment, ftypAtom, moovAtom, alt);
				}
			}

			const foundTracks = this.#getAllTracksInMoov(moovAtom);
			const foundHandlers = foundTracks.map((t) => t.handler || "unknown").join(", ");
			throw new Error(
				`${trackType} track not found in moov atom. ` +
					`Looking for handler: "${targetHandler}", but found: [${foundHandlers}]. ` +
					`The init segment should contain all tracks.`,
			);
		}

		// Reconstruct moov atom with only the target track
		const newMoov = this.#rebuildMoovWithSingleTrack(moovAtom, trakAtom, targetHandler);

		// Combine ftyp (if present) + new moov
		const result: Uint8Array[] = [];
		if (ftypAtom) {
			result.push(ftypAtom);
		}
		result.push(newMoov);

		const totalSize = result.reduce((sum, arr) => sum + arr.length, 0);
		const combined = new Uint8Array(totalSize);
		let writeOffset = 0;
		for (const arr of result) {
			combined.set(arr, writeOffset);
			writeOffset += arr.length;
		}

		return combined;
	}

	#extractTrackInitSegmentWithHandler(
		_fullInitSegment: Uint8Array,
		ftypAtom: Uint8Array | null,
		moovAtom: Uint8Array,
		handlerType: string,
	): Uint8Array {
		const trakAtom = this.#findTrackInMoov(moovAtom, handlerType);
		if (!trakAtom) {
			throw new Error(`Track with handler "${handlerType}" not found`);
		}

		const newMoov = this.#rebuildMoovWithSingleTrack(moovAtom, trakAtom, handlerType);

		const result: Uint8Array[] = [];
		if (ftypAtom) {
			result.push(ftypAtom);
		}
		result.push(newMoov);

		const totalSize = result.reduce((sum, arr) => sum + arr.length, 0);
		const combined = new Uint8Array(totalSize);
		let writeOffset = 0;
		for (const arr of result) {
			combined.set(arr, writeOffset);
			writeOffset += arr.length;
		}

		return combined;
	}

	#getAllTracksInMoov(moovAtom: Uint8Array): Array<{ handler: string | null }> {
		const tracks: Array<{ handler: string | null }> = [];
		let offset = 8; // Skip moov header

		while (offset + 8 <= moovAtom.length) {
			const size =
				(moovAtom[offset] << 24) |
				(moovAtom[offset + 1] << 16) |
				(moovAtom[offset + 2] << 8) |
				moovAtom[offset + 3];
			const type = String.fromCharCode(
				moovAtom[offset + 4],
				moovAtom[offset + 5],
				moovAtom[offset + 6],
				moovAtom[offset + 7],
			);

			if (type === "trak") {
				const trakAtom = moovAtom.slice(offset, offset + size);
				const handler = this.#getHandlerType(trakAtom);
				tracks.push({ handler: handler || null });
			}

			if (size < 8 || size === 0) break;
			offset += size;
		}

		return tracks;
	}

	#getHandlerType(trakAtom: Uint8Array): string | null {
		let offset = 8; // Skip trak header

		while (offset + 8 <= trakAtom.length) {
			const size =
				(trakAtom[offset] << 24) |
				(trakAtom[offset + 1] << 16) |
				(trakAtom[offset + 2] << 8) |
				trakAtom[offset + 3];
			const type = String.fromCharCode(
				trakAtom[offset + 4],
				trakAtom[offset + 5],
				trakAtom[offset + 6],
				trakAtom[offset + 7],
			);

			if (type === "mdia") {
				const mdiaAtom = trakAtom.slice(offset, offset + size);
				let mdiaOffset = 8;
				while (mdiaOffset + 8 <= mdiaAtom.length) {
					const hdlrSize =
						(mdiaAtom[mdiaOffset] << 24) |
						(mdiaAtom[mdiaOffset + 1] << 16) |
						(mdiaAtom[mdiaOffset + 2] << 8) |
						mdiaAtom[mdiaOffset + 3];
					const hdlrType = String.fromCharCode(
						mdiaAtom[mdiaOffset + 4],
						mdiaAtom[mdiaOffset + 5],
						mdiaAtom[mdiaOffset + 6],
						mdiaAtom[mdiaOffset + 7],
					);

					if (hdlrType === "hdlr") {
						if (mdiaOffset + 24 <= mdiaAtom.length) {
							const handlerTypeBytes = String.fromCharCode(
								mdiaAtom[mdiaOffset + 16],
								mdiaAtom[mdiaOffset + 17],
								mdiaAtom[mdiaOffset + 18],
								mdiaAtom[mdiaOffset + 19],
							);
							return handlerTypeBytes;
						}
					}

					if (hdlrSize < 8 || hdlrSize === 0) break;
					mdiaOffset += hdlrSize;
				}
			}

			if (size < 8 || size === 0) break;
			offset += size;
		}

		return null;
	}

	#findTrackInMoov(moovAtom: Uint8Array, handlerType: string): Uint8Array | null {
		let offset = 8; // Skip moov header

		while (offset + 8 <= moovAtom.length) {
			const size =
				(moovAtom[offset] << 24) |
				(moovAtom[offset + 1] << 16) |
				(moovAtom[offset + 2] << 8) |
				moovAtom[offset + 3];
			const type = String.fromCharCode(
				moovAtom[offset + 4],
				moovAtom[offset + 5],
				moovAtom[offset + 6],
				moovAtom[offset + 7],
			);

			if (type === "trak") {
				const trakAtom = moovAtom.slice(offset, offset + size);
				if (this.#trakHasHandler(trakAtom, handlerType)) {
					return trakAtom;
				}
			}

			if (size < 8 || size === 0) break;
			offset += size;
		}

		return null;
	}

	#trakHasHandler(trakAtom: Uint8Array, handlerType: string): boolean {
		const foundHandler = this.#getHandlerType(trakAtom);
		return foundHandler === handlerType;
	}

	#rebuildMoovWithSingleTrack(moovAtom: Uint8Array, trakAtom: Uint8Array, targetHandler: string): Uint8Array {
		const parts: Uint8Array[] = [];
		let offset = 8; // Skip moov header

		const trackId = this.#getTrackId(trakAtom);

		while (offset + 8 <= moovAtom.length) {
			const size =
				(moovAtom[offset] << 24) |
				(moovAtom[offset + 1] << 16) |
				(moovAtom[offset + 2] << 8) |
				moovAtom[offset + 3];
			const type = String.fromCharCode(
				moovAtom[offset + 4],
				moovAtom[offset + 5],
				moovAtom[offset + 6],
				moovAtom[offset + 7],
			);

			if (type === "mvhd") {
				parts.push(moovAtom.slice(offset, offset + size));
			} else if (type === "trak") {
				const trak = moovAtom.slice(offset, offset + size);
				if (this.#trakHasHandler(trak, targetHandler)) {
					parts.push(trak);
				}
			} else if (type === "mvex") {
				const mvexAtom = moovAtom.slice(offset, offset + size);
				const rebuiltMvex = this.#rebuildMvexWithSingleTrack(mvexAtom, trackId);
				if (rebuiltMvex) {
					parts.push(rebuiltMvex);
				}
			}

			if (size < 8 || size === 0) break;
			offset += size;
		}

		const totalSize = 8 + parts.reduce((sum, arr) => sum + arr.length, 0);
		const newMoov = new Uint8Array(totalSize);

		newMoov[0] = (totalSize >>> 24) & 0xff;
		newMoov[1] = (totalSize >>> 16) & 0xff;
		newMoov[2] = (totalSize >>> 8) & 0xff;
		newMoov[3] = totalSize & 0xff;
		newMoov[4] = 0x6d; // 'm'
		newMoov[5] = 0x6f; // 'o'
		newMoov[6] = 0x6f; // 'o'
		newMoov[7] = 0x76; // 'v'

		let writeOffset = 8;
		for (const part of parts) {
			newMoov.set(part, writeOffset);
			writeOffset += part.length;
		}

		return newMoov;
	}

	#getTrackId(trakAtom: Uint8Array): number {
		let offset = 8; // Skip trak header

		while (offset + 8 <= trakAtom.length) {
			const size =
				(trakAtom[offset] << 24) |
				(trakAtom[offset + 1] << 16) |
				(trakAtom[offset + 2] << 8) |
				trakAtom[offset + 3];
			const type = String.fromCharCode(
				trakAtom[offset + 4],
				trakAtom[offset + 5],
				trakAtom[offset + 6],
				trakAtom[offset + 7],
			);

			if (type === "tkhd") {
				const version = trakAtom[offset + 8];
				const trackIdOffset = version === 1 ? 24 : 16;
				if (offset + trackIdOffset + 4 <= trakAtom.length) {
					return (
						(trakAtom[offset + trackIdOffset] << 24) |
						(trakAtom[offset + trackIdOffset + 1] << 16) |
						(trakAtom[offset + trackIdOffset + 2] << 8) |
						trakAtom[offset + trackIdOffset + 3]
					);
				}
			}

			if (size < 8 || size === 0) break;
			offset += size;
		}

		return 0;
	}

	#rebuildMvexWithSingleTrack(mvexAtom: Uint8Array, trackId: number): Uint8Array | null {
		const parts: Uint8Array[] = [];
		let offset = 8; // Skip mvex header

		while (offset + 8 <= mvexAtom.length) {
			const size =
				(mvexAtom[offset] << 24) |
				(mvexAtom[offset + 1] << 16) |
				(mvexAtom[offset + 2] << 8) |
				mvexAtom[offset + 3];
			const type = String.fromCharCode(
				mvexAtom[offset + 4],
				mvexAtom[offset + 5],
				mvexAtom[offset + 6],
				mvexAtom[offset + 7],
			);

			if (type === "trex") {
				if (offset + 16 <= mvexAtom.length) {
					const trexTrackId =
						(mvexAtom[offset + 12] << 24) |
						(mvexAtom[offset + 13] << 16) |
						(mvexAtom[offset + 14] << 8) |
						mvexAtom[offset + 15];
					if (trexTrackId === trackId) {
						parts.push(mvexAtom.slice(offset, offset + size));
					}
				}
			}

			if (size < 8 || size === 0) break;
			offset += size;
		}

		if (parts.length === 0) {
			return null;
		}

		const totalSize = 8 + parts.reduce((sum, arr) => sum + arr.length, 0);
		const newMvex = new Uint8Array(totalSize);

		newMvex[0] = (totalSize >>> 24) & 0xff;
		newMvex[1] = (totalSize >>> 16) & 0xff;
		newMvex[2] = (totalSize >>> 8) & 0xff;
		newMvex[3] = totalSize & 0xff;
		newMvex[4] = 0x6d; // 'm'
		newMvex[5] = 0x76; // 'v'
		newMvex[6] = 0x65; // 'e'
		newMvex[7] = 0x78; // 'x'

		let writeOffset = 8;
		for (const part of parts) {
			newMvex.set(part, writeOffset);
			writeOffset += part.length;
		}

		return newMvex;
	}

	#processVideoQueue(): void {
		if (!this.#videoSourceBuffer || this.#videoSourceBuffer.updating || this.#videoAppendQueue.length === 0) {
			return;
		}

		if (this.#mediaSource?.readyState !== "open") {
			return;
		}

		// Wait if any SourceBuffer is updating (dash.js pattern)
		if (this.#isBufferUpdating()) {
			return;
		}

		const fragment = this.#videoAppendQueue.shift();
		if (!fragment) return;

		try {
			this.#videoSourceBuffer.appendBuffer(fragment as BufferSource);
			this.#stats.update((current) => {
				const newCount = (current?.frameCount ?? 0) + 1;
				if (newCount === 1 || newCount % 10 === 0) {
					console.log(`[MSE] Appended video fragment ${newCount}, size: ${fragment.byteLength} bytes`);
				}
				return {
					frameCount: newCount,
					timestamp: current?.timestamp ?? 0,
					bytesReceived: (current?.bytesReceived ?? 0) + fragment.byteLength,
				};
			});
		} catch (error) {
			// Let browser handle buffer management - just log the error
			if (error instanceof DOMException && error.name === "QuotaExceededError") {
				console.warn("[MSE] QuotaExceededError - browser will manage buffer automatically");
				// Put fragment back in queue to retry later
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

		// Wait if any SourceBuffer is updating (dash.js pattern)
		if (this.#isBufferUpdating()) {
			return;
		}

		const fragment = this.#audioAppendQueue.shift();
		if (!fragment) return;

		try {
			this.#audioSourceBuffer.appendBuffer(fragment as BufferSource);
		} catch (error) {
			// Let browser handle buffer management - just log the error
			if (error instanceof DOMException && error.name === "QuotaExceededError") {
				console.warn("[MSE] QuotaExceededError for audio - browser will manage buffer automatically");
				// Put fragment back in queue to retry later
				this.#audioAppendQueue.unshift(fragment);
			} else {
				console.error("[MSE] Error appending audio fragment:", error);
			}
		}
	}

	// Backward compatibility - delegates to appendVideoFragment
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

		// Briefly wait for audio SourceBuffer so we don't hit Chrome's quota race.
		console.log("[MSE] Checking if audio SourceBuffer will be added...");
		for (let i = 0; i < 10; i++) {
			// up to ~1s
			if (this.#audioSourceBuffer || (this.#mediaSource && this.#mediaSource.sourceBuffers.length >= 2)) {
				console.log("[MSE] Audio SourceBuffer detected, proceeding with video");
				break;
			}
			await new Promise((resolve) => setTimeout(resolve, 100));
		}

		const sub = broadcast.subscribe(name, PRIORITY.video);
		effect.cleanup(() => sub.close());

		const consumer = new Frame.Consumer(sub, {
			latency: this.latency,
			container: "cmaf",
		});
		effect.cleanup(() => consumer.close());

		// Init segment must be in catalog for CMAF
		if (!config.initSegment) {
			throw new Error("Init segment is required in catalog for CMAF playback");
		}

		// Decode base64 string to Uint8Array
		const binaryString = atob(config.initSegment);
		const fullInitSegment = new Uint8Array(binaryString.length);
		for (let i = 0; i < binaryString.length; i++) {
			fullInitSegment[i] = binaryString.charCodeAt(i);
		}

		// Extract video-specific init segment
		const videoInitSegment = this.#extractTrackInitSegment(fullInitSegment, "video");

		// Append init segment and wait for completion
		if (!this.#videoSourceBuffer) {
			throw new Error("Video SourceBuffer not available");
		}

		const videoSourceBuffer = this.#videoSourceBuffer;
		console.log("[MSE] Appending video init segment, size:", videoInitSegment.byteLength, "bytes");
		await new Promise<void>((resolve, reject) => {
			const onUpdateEnd = () => {
				videoSourceBuffer.removeEventListener("updateend", onUpdateEnd);
				videoSourceBuffer.removeEventListener("error", onError);
				console.log("[MSE] Video init segment appended successfully");
				resolve();
			};

			const onError = (e: Event) => {
				videoSourceBuffer.removeEventListener("updateend", onUpdateEnd);
				videoSourceBuffer.removeEventListener("error", onError);
				const error = e as ErrorEvent;
				console.error("[MSE] Video SourceBuffer error appending init segment:", error);
				reject(new Error(`Video SourceBuffer error: ${error.message || "unknown error"}`));
			};

			videoSourceBuffer.addEventListener("updateend", onUpdateEnd, { once: true });
			videoSourceBuffer.addEventListener("error", onError, { once: true });

			try {
				videoSourceBuffer.appendBuffer(videoInitSegment as BufferSource);
			} catch (error) {
				videoSourceBuffer.removeEventListener("updateend", onUpdateEnd);
				videoSourceBuffer.removeEventListener("error", onError);
				console.error("[MSE] Error calling appendBuffer on video init segment:", error);
				reject(error);
			}
		});

		// Helper function to detect init segment
		function isInitSegmentData(data: Uint8Array): boolean {
			if (data.length < 8) return false;

			let offset = 0;
			const len = data.length;

			while (offset + 8 <= len) {
				const size =
					(data[offset] << 24) | (data[offset + 1] << 16) | (data[offset + 2] << 8) | data[offset + 3];

				const type = String.fromCharCode(
					data[offset + 4],
					data[offset + 5],
					data[offset + 6],
					data[offset + 7],
				);

				if (type === "ftyp" || type === "moov") return true;

				if (size < 8 || size === 0) break;
				offset += size;
			}

			return false;
		}

		// Read fragments and append to SourceBuffer
		// Each fragment is already a complete CMAF segment (moof+mdat), so we can append individually
		// This reduces latency and memory usage compared to batching by group
		console.log("[MSE] Starting to read video fragments from track");
		effect.spawn(async () => {
			let frameCount = 0;

			for (;;) {
				const frame = await Promise.race([consumer.decode(), effect.cancel]);
				if (!frame) {
					console.log(`[MSE] Video track ended, processed ${frameCount} frames`);
					break;
				}

				frameCount++;
				if (frameCount === 1 || frameCount % 10 === 0) {
					console.log(`[MSE] Processing video frame ${frameCount}, group: ${frame.group}`);
				}

				// Skip any init segments that might come from track
				if (isInitSegmentData(frame.data)) {
					continue;
				}

				// Append fragment immediately - each fragment is a complete CMAF segment
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
		// Check if audio SourceBuffer was initialized
		// If not, allow video-only playback
		if (!this.#audioSourceBuffer) {
			console.log("[MSE] Audio SourceBuffer not available, skipping audio track (video-only playback)");
			return;
		}

		// Init segment must be in catalog for CMAF
		if (!config.initSegment) {
			throw new Error("Init segment is required in catalog for CMAF audio playback");
		}

		// Decode base64 string to Uint8Array
		const binaryString = atob(config.initSegment);
		const fullInitSegment = new Uint8Array(binaryString.length);
		for (let i = 0; i < binaryString.length; i++) {
			fullInitSegment[i] = binaryString.charCodeAt(i);
		}

		// Extract audio-specific init segment
		const audioInitSegment = this.#extractTrackInitSegment(fullInitSegment, "audio");

		// Append init segment
		await this.appendAudioFragment(audioInitSegment);

		// Check if enabled
		const isEnabled = enabled ? effect.get(enabled) : true;
		if (!isEnabled) {
			return;
		}

		const sub = broadcast.subscribe(name, catalog.priority);
		effect.cleanup(() => sub.close());

		const consumer = new Frame.Consumer(sub, {
			latency: this.latency,
			container: "cmaf",
		});
		effect.cleanup(() => consumer.close());

		function hasMoovAtom(data: Uint8Array): boolean {
			let offset = 0;
			const len = data.length;
			while (offset + 8 <= len) {
				const size =
					(data[offset] << 24) | (data[offset + 1] << 16) | (data[offset + 2] << 8) | data[offset + 3];
				const type = String.fromCharCode(
					data[offset + 4],
					data[offset + 5],
					data[offset + 6],
					data[offset + 7],
				);
				if (type === "moov") return true;
				if (size < 8 || size === 0) break;
				offset += size;
			}
			return false;
		}

		effect.spawn(async () => {
			for (;;) {
				const frame = await Promise.race([consumer.decode(), effect.cancel]);
				if (!frame) {
					break;
				}

				if (this.#mediaSource?.readyState === "closed") {
					break;
				}

				// Skip any init segments
				if (hasMoovAtom(frame.data)) {
					continue;
				}

				// Append fragment immediately - each fragment is a complete CMAF segment
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

		// Store references before resetting
		const audioSourceBuffer = this.#audioSourceBuffer;
		const videoSourceBuffer = this.#videoSourceBuffer;
		const mediaSource = this.#mediaSource;

		this.#audioSourceBuffer = undefined; // Reset audio SourceBuffer reference

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
