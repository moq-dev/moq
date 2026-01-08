import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import type * as Catalog from "../../catalog";
import * as Frame from "../../frame";
import { PRIORITY } from "../../publish/priority";
import type * as Time from "../../time";
import * as Mime from "../../util/mime";

// The types in VideoDecoderConfig that cause a hard reload.
// ex. codedWidth/Height are optional and can be changed in-band, so we don't want to trigger a reload.
// This way we can keep the current subscription active.
// Note: We keep codedWidth/Height as optional for logging, but set them to undefined to avoid reloads.
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
	#sourceBuffer?: SourceBuffer;

	// Queue of fragments waiting to be added
	// Maximum limit to prevent infinite growth in live streaming
	#appendQueue: Uint8Array[] = [];
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

	async initialize(config: RequiredDecoderConfig): Promise<void> {
		const mimeType = Mime.buildVideoMimeType(config);
		if (!mimeType) {
			throw new Error(`Unsupported codec for MSE: ${config.codec}`);
		}

		this.#video = document.createElement("video");
		this.#video.style.display = "none";
		this.#video.playsInline = true;
		this.#video.muted = true; // Required for autoplay
		document.body.appendChild(this.#video);

		// Note: In live streaming, "waiting" events are common and normal as the video waits for new data
		this.#video.addEventListener("waiting", () => {
			// Silently handle - this is expected in live streaming
		});

		this.#video.addEventListener("ended", () => {
			if (!this.#video) return;
			const videoBuffered = this.#video.buffered;
			const current = this.#video.currentTime;

			if (videoBuffered && videoBuffered.length > 0) {
				const lastRange = videoBuffered.length - 1;
				const end = videoBuffered.end(lastRange);
				if (current < end) {
					this.#video.currentTime = current;
					this.#video.play().catch((err) => console.error("[MSE] Failed to resume after ended:", err));
				}
			}
		});

		this.#video.addEventListener("timeupdate", () => {
			if (!this.#video) return;
			const videoBuffered = this.#video.buffered;
			const current = this.#video.currentTime;
			if (videoBuffered && videoBuffered.length > 0) {
				const lastRange = videoBuffered.length - 1;
				const end = videoBuffered.end(lastRange);
				const remaining = end - current;
				if (remaining <= 0.1 && this.#video.paused) {
					this.#video.play().catch((err) => console.error("[MSE] Failed to resume playback:", err));
				}
			}
		});

		this.#mediaSource = new MediaSource();
		const url = URL.createObjectURL(this.#mediaSource);
		this.#video.src = url;
		this.#video.currentTime = 0;

		await new Promise<void>((resolve, reject) => {
			const timeout = setTimeout(() => {
				reject(new Error("MediaSource sourceopen timeout"));
			}, 5000);

			this.#mediaSource?.addEventListener(
				"sourceopen",
				() => {
					clearTimeout(timeout);
					try {
						this.#sourceBuffer = this.#mediaSource?.addSourceBuffer(mimeType);
						if (!this.#sourceBuffer) {
							reject(new Error("Failed to create SourceBuffer"));
							return;
						}
						this.#setupSourceBuffer();
						resolve();
					} catch (error) {
						reject(error);
					}
				},
				{ once: true },
			);

			this.#mediaSource?.addEventListener("error", (e) => {
				clearTimeout(timeout);
				reject(new Error(`MediaSource error: ${e}`));
			});
		});

		this.#startFrameCapture();
	}

	#setupSourceBuffer(): void {
		if (!this.#sourceBuffer) return;

		this.#sourceBuffer.addEventListener("updateend", () => {
			this.#processAppendQueue();
		});

		this.#sourceBuffer.addEventListener("error", (e) => {
			console.error("SourceBuffer error:", e);
		});
	}

	#startFrameCapture(): void {
		if (!this.#video) return;

		const captureFrame = () => {
			if (!this.#video) return;

			try {
				const frame = new VideoFrame(this.#video, {
					timestamp: this.#video.currentTime * 1_000_000, // Convert to microseconds
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
				}
			} catch (error) {
				console.error("Error capturing frame:", error);
			}

			if (this.#video.requestVideoFrameCallback) {
				this.#frameCallbackId = this.#video.requestVideoFrameCallback(captureFrame);
			} else {
				// Fallback: use requestAnimationFrame
				this.#frameCallbackId = requestAnimationFrame(captureFrame) as unknown as number;
			}
		};

		if (this.#video.requestVideoFrameCallback) {
			this.#frameCallbackId = this.#video.requestVideoFrameCallback(captureFrame);
		} else {
			this.#frameCallbackId = requestAnimationFrame(captureFrame) as unknown as number;
		}
	}

	async appendFragment(fragment: Uint8Array): Promise<void> {
		if (!this.#sourceBuffer || !this.#mediaSource) {
			throw new Error("SourceBuffer not initialized");
		}
		if (this.#appendQueue.length >= SourceMSE.MAX_QUEUE_SIZE) {
			const discarded = this.#appendQueue.shift();
			console.warn(
				`[MSE] Queue full (${SourceMSE.MAX_QUEUE_SIZE}), discarding oldest fragment (${discarded?.byteLength ?? 0} bytes)`,
			);
		}

		const copy = new Uint8Array(fragment);
		this.#appendQueue.push(copy);

		this.#processAppendQueue();
	}

	#concatenateFragments(fragments: Uint8Array[]): Uint8Array {
		if (fragments.length === 1) {
			return fragments[0];
		}

		const totalSize = fragments.reduce((sum, frag) => sum + frag.byteLength, 0);
		const result = new Uint8Array(totalSize);
		let offset = 0;
		for (const fragment of fragments) {
			result.set(fragment, offset);
			offset += fragment.byteLength;
		}

		return result;
	}

	#processAppendQueue(): void {
		if (!this.#sourceBuffer || this.#sourceBuffer.updating || this.#appendQueue.length === 0) {
			return;
		}

		if (this.#mediaSource?.readyState !== "open") {
			console.error(`[MSE] MediaSource not open: ${this.#mediaSource?.readyState}`);
			return;
		}

		const fragment = this.#appendQueue.shift();
		if (!fragment) return;

		try {
			// appendBuffer accepts BufferSource (ArrayBuffer or ArrayBufferView)
			this.#sourceBuffer.appendBuffer(fragment as BufferSource);

			this.#stats.update((current) => ({
				frameCount: current?.frameCount ?? 0,
				timestamp: current?.timestamp ?? 0,
				bytesReceived: (current?.bytesReceived ?? 0) + fragment.byteLength,
			}));
		} catch (error) {
			console.error("[MSE] Error appending fragment:", error);
			console.error("[MSE] SourceBuffer state:", {
				updating: this.#sourceBuffer.updating,
				buffered: this.#sourceBuffer.buffered.length,
			});
			console.error("[MSE] MediaSource state:", {
				readyState: this.#mediaSource.readyState,
				duration: this.#mediaSource.duration,
			});
		}
	}

	async runTrack(
		effect: Effect,
		broadcast: Moq.Broadcast,
		name: string,
		config: RequiredDecoderConfig,
	): Promise<void> {
		await this.initialize(config);

		const sub = broadcast.subscribe(name, PRIORITY.video);
		effect.cleanup(() => sub.close());

		const consumer = new Frame.Consumer(sub, {
			latency: this.latency,
			container: "fmp4", // CMAF fragments
		});
		effect.cleanup(() => consumer.close());

		effect.spawn(async () => {
			if (!this.#video) return;

			await new Promise<void>((resolve) => {
				let checkCount = 0;
				const maxChecks = 100; // 10 seconds max wait
				let hasSeeked = false;

				const checkReady = () => {
					checkCount++;
					if (this.#video) {
						const videoBuffered = this.#video.buffered;
						const hasBufferedData = videoBuffered && videoBuffered.length > 0;
						const currentTime = this.#video.currentTime;
						const isTimeBuffered =
							hasBufferedData &&
							videoBuffered.start(0) <= currentTime &&
							currentTime < videoBuffered.end(videoBuffered.length - 1);

						if (hasBufferedData && !isTimeBuffered && !hasSeeked) {
							const seekTime = videoBuffered.start(0);
							this.#video.currentTime = seekTime;
							hasSeeked = true;
							setTimeout(checkReady, 100);
							return;
						}

						if (this.#video.readyState >= HTMLMediaElement.HAVE_FUTURE_DATA) {
							this.#video
								.play()
								.then(() => {
									resolve();
								})
								.catch((error) => {
									console.error("[MSE] Video play() failed:", error);
									resolve();
								});
						} else if (hasBufferedData && checkCount >= 10) {
							// If we have buffered data but readyState hasn't advanced, try playing anyway after 1 second
							this.#video
								.play()
								.then(() => {
									resolve();
								})
								.catch((error) => {
									console.error("[MSE] Video play() failed:", error);
									if (checkCount < maxChecks) {
										setTimeout(checkReady, 100);
									} else {
										resolve();
									}
								});
						} else if (checkCount >= maxChecks) {
							this.#video
								.play()
								.then(() => {
									resolve();
								})
								.catch(() => {
									resolve();
								});
						} else {
							setTimeout(checkReady, 100);
						}
					}
				};
				checkReady();
			});
		});

		// Track if we've received the init segment (ftyp+moov or moov)
		let initSegmentReceived = false;

		// Helper function to detect init segment (ftyp or moov atom)
		// The init segment may start with "ftyp" followed by "moov", or just "moov"
		function isInitSegmentData(data: Uint8Array): boolean {
			if (data.length < 8) return false;

			let offset = 0;
			const len = data.length;

			while (offset + 8 <= len) {
				// Atom size (big endian)
				const size =
					(data[offset] << 24) | (data[offset + 1] << 16) | (data[offset + 2] << 8) | data[offset + 3];

				const type = String.fromCharCode(
					data[offset + 4],
					data[offset + 5],
					data[offset + 6],
					data[offset + 7],
				);

				// Init segment contains either "ftyp" or "moov" atoms
				if (type === "ftyp" || type === "moov") return true;

				if (size < 8 || size === 0) break;
				offset += size;
			}

			return false;
		}

		// Read fragments and append to SourceBuffer
		// MSE requires complete GOPs to be appended in a single operation
		// We group fragments by MOQ group (which corresponds to GOPs) before appending
		effect.spawn(async () => {
			let currentGroup: number | undefined;
			let gopFragments: Uint8Array[] = []; // Accumulate fragments for current GOP

			for (;;) {
				const frame = await Promise.race([consumer.decode(), effect.cancel]);
				if (!frame) {
					if (gopFragments.length > 0 && initSegmentReceived) {
						const gopData = this.#concatenateFragments(gopFragments);
						await this.appendFragment(gopData);
						gopFragments = [];
					}
					break;
				}

				const containsInitSegmentData = isInitSegmentData(frame.data);
				const isInitSegment = containsInitSegmentData && !initSegmentReceived;

				if (isInitSegment) {
					if (gopFragments.length > 0 && initSegmentReceived) {
						const gopData = this.#concatenateFragments(gopFragments);
						await this.appendFragment(gopData);
						gopFragments = [];
					}

					await this.appendFragment(frame.data);
					initSegmentReceived = true;
					continue;
				}

				if (!initSegmentReceived) {
					continue;
				}

				if (currentGroup !== undefined && frame.group !== currentGroup) {
					if (gopFragments.length > 0) {
						const gopData = this.#concatenateFragments(gopFragments);
						await this.appendFragment(gopData);
						gopFragments = [];
					}
				}

				if (currentGroup === undefined || frame.group !== currentGroup) {
					currentGroup = frame.group;
					gopFragments = [];
				}

				gopFragments.push(frame.data);

				if (gopFragments.length >= 1) {
					const gopData = this.#concatenateFragments(gopFragments);
					await this.appendFragment(gopData);
					gopFragments = [];
				}
			}
		});
	}

	close(): void {
		this.#appendQueue = [];

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

		if (this.#sourceBuffer && this.#mediaSource) {
			try {
				if (this.#sourceBuffer.updating) {
					this.#sourceBuffer.abort();
				}
				if (this.#mediaSource.readyState === "open") {
					this.#mediaSource.endOfStream();
				}
			} catch (error) {
				console.error("Error closing SourceBuffer:", error);
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
