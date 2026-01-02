import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import type * as Catalog from "../../catalog";
import * as Frame from "../../frame";
import { PRIORITY } from "../../publish/priority";
import type * as Time from "../../time";
import * as Mime from "../../util/mime";

// The types in VideoDecoderConfig that cause a hard reload.
type RequiredDecoderConfig = Omit<Catalog.VideoConfig, "codedWidth" | "codedHeight">;

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

	// Cola de fragmentos esperando ser añadidos
	// Límite máximo para evitar crecimiento infinito en live streaming
	#appendQueue: Uint8Array[] = [];
	static readonly MAX_QUEUE_SIZE = 10; // Máximo de fragmentos en cola

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
		// Build MIME type from codec
		const mimeType = Mime.buildVideoMimeType(config);
		if (!mimeType) {
			throw new Error(`Unsupported codec for MSE: ${config.codec}`);
		}
		console.log(`[MSE] Initializing with MIME type: ${mimeType}, codec: ${config.codec}`);

		// Create hidden video element
		this.#video = document.createElement("video");
		this.#video.style.display = "none";
		this.#video.playsInline = true;
		this.#video.muted = true; // Required for autoplay
		document.body.appendChild(this.#video);

		// Listen for stalled event (when video runs out of data)
		this.#video.addEventListener("waiting", () => {
			if (!this.#video) return;
			const buffered = this.#sourceBuffer?.buffered;
			const videoBuffered = this.#video.buffered;
			const current = this.#video.currentTime;
			const sourceBufferInfo = buffered && buffered.length > 0
				? `${buffered.length} ranges, last: ${buffered.end(buffered.length - 1).toFixed(2)}s`
				: "no ranges";
			const videoBufferedInfo = videoBuffered && videoBuffered.length > 0
				? `${videoBuffered.length} ranges, last: ${videoBuffered.end(videoBuffered.length - 1).toFixed(2)}s`
				: "no ranges";
			console.warn(`[MSE] Video waiting for data (stalled) at ${current.toFixed(2)}s, SourceBuffer: ${sourceBufferInfo}, Video: ${videoBufferedInfo}`);
		});

		// Listen for ended event
		this.#video.addEventListener("ended", () => {
			if (!this.#video) return;
			const buffered = this.#sourceBuffer?.buffered;
			const videoBuffered = this.#video.buffered;
			const current = this.#video.currentTime;
			const sourceBufferInfo = buffered && buffered.length > 0
				? `${buffered.length} ranges, last: ${buffered.end(buffered.length - 1).toFixed(2)}s`
				: "no ranges";
			const videoBufferedInfo = videoBuffered && videoBuffered.length > 0
				? `${videoBuffered.length} ranges, last: ${videoBuffered.end(videoBuffered.length - 1).toFixed(2)}s`
				: "no ranges";
			console.warn(`[MSE] Video ended at ${current.toFixed(2)}s - SourceBuffer: ${sourceBufferInfo}, Video: ${videoBufferedInfo}`);
			// For live streams, try to resume playback if we have buffered data
			if (videoBuffered && videoBuffered.length > 0) {
				const lastRange = videoBuffered.length - 1;
				const end = videoBuffered.end(lastRange);
				if (current < end) {
					console.warn(`[MSE] Video ended but has buffered data up to ${end.toFixed(2)}s, seeking to current time`);
					this.#video.currentTime = current;
					this.#video.play().catch(err => console.error("[MSE] Failed to resume after ended:", err));
				}
			}
		});

		// Listen for timeupdate to monitor playback
		this.#video.addEventListener("timeupdate", () => {
			if (!this.#video) return;
			const buffered = this.#sourceBuffer?.buffered;
			const videoBuffered = this.#video.buffered;
			const current = this.#video.currentTime;
			// Check video buffered ranges (more accurate for playback)
			if (videoBuffered && videoBuffered.length > 0) {
				const lastRange = videoBuffered.length - 1;
				const end = videoBuffered.end(lastRange);
				const remaining = end - current;
				// Log warning if we're getting close to the end of buffered data
				if (remaining < 1.0 && remaining > 0) {
					console.warn(`[MSE] Video approaching end of buffered data: ${remaining.toFixed(2)}s remaining (current: ${current.toFixed(2)}s, buffered up to: ${end.toFixed(2)}s)`);
				}
				// If we've reached the end and video is paused, try to resume
				if (remaining <= 0.1 && this.#video.paused) {
					console.warn(`[MSE] Video reached end of buffered data, attempting to resume...`);
					this.#video.play().catch(err => console.error("[MSE] Failed to resume playback:", err));
				}
			} else if (buffered && buffered.length > 0) {
				// SourceBuffer has data but video doesn't see it - this is a problem
				const lastRange = buffered.length - 1;
				const end = buffered.end(lastRange);
				const remaining = end - current;
				if (remaining < 1.0 && remaining > 0) {
					console.warn(`[MSE] Video approaching end of SourceBuffer data (video doesn't see it): ${remaining.toFixed(2)}s remaining`);
				}
			}
		});

		// Create MediaSource
		this.#mediaSource = new MediaSource();
		const url = URL.createObjectURL(this.#mediaSource);
		this.#video.src = url;
		
		// Set initial time to 0 to ensure playback starts from the beginning
		this.#video.currentTime = 0;

		// Wait for sourceopen event
		await new Promise<void>((resolve, reject) => {
			const timeout = setTimeout(() => {
				reject(new Error("MediaSource sourceopen timeout"));
			}, 5000);

			this.#mediaSource!.addEventListener(
				"sourceopen",
				() => {
					clearTimeout(timeout);
					try {
						// Create SourceBuffer
						this.#sourceBuffer = this.#mediaSource!.addSourceBuffer(mimeType);
						this.#setupSourceBuffer();
						resolve();
					} catch (error) {
						reject(error);
					}
				},
				{ once: true },
			);

			this.#mediaSource!.addEventListener("error", (e) => {
				clearTimeout(timeout);
				reject(new Error(`MediaSource error: ${e}`));
			});
		});

		// Start capturing frames from video element
		this.#startFrameCapture();
	}

	#setupSourceBuffer(): void {
		if (!this.#sourceBuffer) return;

		// Handle updateend events
		this.#sourceBuffer.addEventListener("updateend", () => {
			// SourceBuffer is ready for more data
			if (this.#sourceBuffer && this.#sourceBuffer.buffered.length > 0) {
				const lastRange = this.#sourceBuffer.buffered.length - 1;
				const start = this.#sourceBuffer.buffered.start(lastRange);
				const end = this.#sourceBuffer.buffered.end(lastRange);
			} else {
				console.log("[MSE] SourceBuffer buffered: 0 ranges (no data buffered yet)");
			}
			if (this.#video) {
				console.log(`[MSE] Video readyState after updateend: ${this.#video.readyState} (HAVE_METADATA=${HTMLMediaElement.HAVE_METADATA}, HAVE_FUTURE_DATA=${HTMLMediaElement.HAVE_FUTURE_DATA})`);
			}
			
			// Procesar la cola cuando termine la operación actual
			this.#processAppendQueue();
		});

		this.#sourceBuffer.addEventListener("error", (e) => {
			console.error("SourceBuffer error:", e);
		});
	}

	#startFrameCapture(): void {
		if (!this.#video) return;

		// Use requestVideoFrameCallback to capture frames
		const captureFrame = () => {
			if (!this.#video) return;

			try {
				// Create VideoFrame from video element
				const frame = new VideoFrame(this.#video, {
					timestamp: this.#video.currentTime * 1_000_000, // Convert to microseconds
				});

				// Update stats
				this.#stats.update((current) => ({
					frameCount: (current?.frameCount ?? 0) + 1,
					timestamp: frame.timestamp,
					bytesReceived: current?.bytesReceived ?? 0,
				}));

				// Update frame signal
				this.frame.update((prev) => {
					prev?.close();
					return frame;
				});

				// Update display size
				if (this.#video.videoWidth && this.#video.videoHeight) {
					this.display.set({
						width: this.#video.videoWidth,
						height: this.#video.videoHeight,
					});
				}

				// Update buffer status
				if (this.#video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
					this.bufferStatus.set({ state: "filled" });
				}
			} catch (error) {
				console.error("Error capturing frame:", error);
			}

			// Schedule next frame capture
			if (this.#video.requestVideoFrameCallback) {
				this.#frameCallbackId = this.#video.requestVideoFrameCallback(captureFrame);
			} else {
				// Fallback: use requestAnimationFrame
				this.#frameCallbackId = requestAnimationFrame(captureFrame) as unknown as number;
			}
		};

		// Start capturing
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

		// Si la cola está llena, descartar el fragmento más antiguo (FIFO)
		// Esto mantiene baja la latencia en live streaming
		if (this.#appendQueue.length >= SourceMSE.MAX_QUEUE_SIZE) {
			const discarded = this.#appendQueue.shift();
			console.warn(`[MSE] Queue full (${SourceMSE.MAX_QUEUE_SIZE}), discarding oldest fragment (${discarded?.byteLength ?? 0} bytes)`);
		}

		// Añadir a la cola en lugar de esperar
		// Crear una copia con ArrayBuffer real (no SharedArrayBuffer)
		const copy = new Uint8Array(fragment);
		this.#appendQueue.push(copy);
		
		// Intentar procesar inmediatamente si está disponible
		this.#processAppendQueue();
	}

	#concatenateFragments(fragments: Uint8Array[]): Uint8Array {
		if (fragments.length === 1) {
			return fragments[0];
		}
		
		// Calculate total size
		const totalSize = fragments.reduce((sum, frag) => sum + frag.byteLength, 0);
		
		// Concatenate all fragments into a single Uint8Array
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

		const fragment = this.#appendQueue.shift()!;
		
		try {
			// appendBuffer accepts BufferSource (ArrayBuffer or ArrayBufferView)
			this.#sourceBuffer.appendBuffer(fragment as BufferSource);
			
			// Update stats
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
			// No reintentamos - el fragmento se descarta
		}
	}

	async runTrack(
		effect: Effect,
		broadcast: Moq.Broadcast,
		name: string,
		config: RequiredDecoderConfig,
	): Promise<void> {
		// Initialize MSE
		await this.initialize(config);

		const sub = broadcast.subscribe(name, PRIORITY.video);
		effect.cleanup(() => sub.close());

		// Create consumer for CMAF fragments
		const consumer = new Frame.Consumer(sub, {
			latency: this.latency,
			container: "fmp4", // CMAF fragments
		});
		effect.cleanup(() => consumer.close());


		// Start playing video when we have enough data
		effect.spawn(async () => {
			if (!this.#video) return;

			// Wait for some data to be buffered
			await new Promise<void>((resolve) => {
				let checkCount = 0;
				const maxChecks = 100; // 10 seconds max wait
				let hasSeeked = false;
				
				const checkReady = () => {
					checkCount++;
					if (this.#video) {
						const bufferedRanges = this.#sourceBuffer?.buffered;
						const videoBuffered = this.#video.buffered;
						const sourceBufferInfo = bufferedRanges && bufferedRanges.length > 0 
							? `${bufferedRanges.length} ranges, last: ${bufferedRanges.start(bufferedRanges.length - 1).toFixed(2)}-${bufferedRanges.end(bufferedRanges.length - 1).toFixed(2)}`
							: "no ranges";
						const videoBufferedInfo = videoBuffered && videoBuffered.length > 0
							? `${videoBuffered.length} ranges, last: ${videoBuffered.start(videoBuffered.length - 1).toFixed(2)}-${videoBuffered.end(videoBuffered.length - 1).toFixed(2)}`
							: "no ranges";
						console.log(`[MSE] Video readyState: ${this.#video.readyState}, SourceBuffer buffered: ${sourceBufferInfo}, Video buffered: ${videoBufferedInfo}, checkCount: ${checkCount}`);
						
						// Check if we have buffered data and if the current time is within the buffered range
						// Use video.buffered instead of sourceBuffer.buffered for checking if video can play
						const hasBufferedData = videoBuffered && videoBuffered.length > 0;
						const currentTime = this.#video.currentTime;
						const isTimeBuffered = hasBufferedData && videoBuffered.start(0) <= currentTime && currentTime < videoBuffered.end(videoBuffered.length - 1);
						
						// If we have buffered data but current time is not in range, seek immediately
						if (hasBufferedData && !isTimeBuffered && !hasSeeked) {
							const seekTime = videoBuffered.start(0);
							this.#video.currentTime = seekTime;
							hasSeeked = true;
							// Continue checking after seek
							setTimeout(checkReady, 100);
							return;
						}
						
						if (this.#video.readyState >= HTMLMediaElement.HAVE_FUTURE_DATA) {
							console.log("[MSE] Video has enough data, attempting to play...");
							this.#video.play().then(() => {
								resolve();
							}).catch((error) => {
								console.error("[MSE] Video play() failed:", error);
								resolve();
							});
						} else if (hasBufferedData && checkCount >= 10) {
							// If we have buffered data but readyState hasn't advanced, try playing anyway after 1 second
							console.warn("[MSE] Video has buffered data but readyState hasn't advanced, attempting to play...");
							this.#video.play().then(() => {
								resolve();
							}).catch((error) => {
								console.error("[MSE] Video play() failed:", error);
								// Continue checking
								if (checkCount < maxChecks) {
									setTimeout(checkReady, 100);
								} else {
									resolve();
								}
							});
						} else if (checkCount >= maxChecks) {
							console.warn("[MSE] Video did not reach HAVE_FUTURE_DATA after 10 seconds, attempting to play anyway...");
							this.#video.play().then(() => {
								resolve();
							}).catch((error) => {
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
				// tamaño del atom (big endian)
				const size =
					(data[offset] << 24) |
					(data[offset + 1] << 16) |
					(data[offset + 2] << 8) |
					data[offset + 3];

				const type = String.fromCharCode(
					data[offset + 4],
					data[offset + 5],
					data[offset + 6],
					data[offset + 7],
				);

				// Init segment contains either "ftyp" or "moov" atoms
				if (type === "ftyp" || type === "moov") return true;

				// Evitar loops infinitos si el size viene roto
				if (size < 8 || size === 0) break;
				offset += size;
			}

			return false;
		}
		
		// Read fragments and append to SourceBuffer
		// MSE requires complete GOPs to be appended in a single operation
		// We group fragments by MOQ group (which corresponds to GOPs) before appending
		effect.spawn(async () => {
			let frameCount = 0;
			let currentGroup: number | undefined = undefined;
			let gopFragments: Uint8Array[] = []; // Accumulate fragments for current GOP


			for (;;) {
				const frame = await Promise.race([consumer.decode(), effect.cancel]);
				if (!frame) {
					// Append any remaining GOP fragments before finishing
					if (gopFragments.length > 0 && initSegmentReceived) {
						const gopData = this.#concatenateFragments(gopFragments);
						await this.appendFragment(gopData);
						gopFragments = [];
					}
					console.log(`[MSE] No more frames, total frames processed: ${frameCount}`);
					break;
				}
				frameCount++;
				console.log(`[MSE] Received frame ${frameCount}: timestamp=${frame.timestamp}, size=${frame.data.byteLength}, group=${frame.group}, keyframe=${frame.keyframe}`);

				// Check if this is the init segment (ftyp+moov or just moov)
				const containsInitSegmentData = isInitSegmentData(frame.data);
				const isInitSegment = containsInitSegmentData && !initSegmentReceived;
				
				if (isInitSegment) {
					// Append any pending GOP before processing init segment
					if (gopFragments.length > 0 && initSegmentReceived) {
						const gopData = this.#concatenateFragments(gopFragments);
						await this.appendFragment(gopData);
						gopFragments = [];
					}

					// This is the init segment (moov), append it first
					console.log("[MSE] Appending init segment...");
					await this.appendFragment(frame.data);
					initSegmentReceived = true;
					console.log("[MSE] Init segment (moov) received and appended");
					continue;
				}

				// This is a regular fragment (moof+mdat)
				if (!initSegmentReceived) {
					console.warn(`[MSE] Received fragment before init segment (timestamp=${frame.timestamp}), skipping`);
					continue;
				}

				// Check if we're starting a new group (new GOP)
				if (currentGroup !== undefined && frame.group !== currentGroup) {
					// Append the complete GOP from previous group
					if (gopFragments.length > 0) {
						const gopData = this.#concatenateFragments(gopFragments);
						console.log(`[MSE] Appending complete GOP (group ${currentGroup}): ${gopFragments.length} fragments, total size=${gopData.byteLength}`);
						await this.appendFragment(gopData);
						gopFragments = [];
					}
				}

				// If this is the first fragment of a new group, start accumulating
				if (currentGroup === undefined || frame.group !== currentGroup) {
					currentGroup = frame.group;
					gopFragments = [];
				}

				gopFragments.push(frame.data);
				console.log(`[MSE] Accumulating fragment for GOP (group ${frame.group}): timestamp=${frame.timestamp}, size=${frame.data.byteLength}, total fragments in GOP=${gopFragments.length}`);

				// For live streaming: append immediately if we have at least one fragment
				// This ensures we don't wait indefinitely for more fragments
				// We'll still group by MOQ group, but append more aggressively
				if (gopFragments.length >= 1) {
					// Append immediately - MSE can handle single fragments if they're complete GOPs
					const gopData = this.#concatenateFragments(gopFragments);
					await this.appendFragment(gopData);
					gopFragments = [];
				} 
			}
		});
	}

	close(): void {
		this.#appendQueue = [];

		// Cancel frame capture
		if (this.#frameCallbackId !== undefined) {
			if (this.#video?.requestVideoFrameCallback) {
				this.#video.cancelVideoFrameCallback(this.#frameCallbackId);
			} else {
				cancelAnimationFrame(this.#frameCallbackId);
			}
		}

		// Close current frame
		this.frame.update((prev) => {
			prev?.close();
			return undefined;
		});

		// Clean up SourceBuffer
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

		// Clean up MediaSource
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

		// Remove video element
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

