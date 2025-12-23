import type * as Catalog from "@moq/hang/catalog";
import { MoqClient } from "./moq-client";

/**
 * MSE-based video player for MOQ streams.
 *
 * Uses MediaSource Extensions for native <video> playback with HLS.js-style
 * live sync strategies (catch-up, hard seek, buffer management).
 *
 * @example
 * ```html
 * <moq-mse-player url="http://localhost:4443/anon" path="bbb" debug></moq-mse-player>
 * ```
 */
export class MoqMsePlayer extends HTMLElement {
	private video: HTMLVideoElement;
	private mediaSource: MediaSource | null = null;
	private videoSourceBuffer: SourceBuffer | null = null;
	private audioSourceBuffer: SourceBuffer | null = null;
	private client: MoqClient | null = null;
	private videoQueue: { data: Uint8Array; trackType: "video" }[] = [];
	private audioQueue: { data: Uint8Array; trackType: "audio" }[] = [];
	private isVideoProcessing = false;
	private isAudioProcessing = false;
	private lastVideoProcessTime = 0;
	private lastAudioProcessTime = 0;
	private userPaused = false;
	private monitorInterval: number | null = null;
	private statusIntervalId: number | null = null;
	private connectionState = "disconnected";
	private reconnectTimer: number | null = null;
	private catalogReceived = false;
	private videoInitReceived = false;
	private audioInitReceived = false;

	// Audio-Video Synchronization State
	private firstVideoTimestamp: number | null = null; // In timescale units
	private firstAudioTimestamp: number | null = null; // In timescale units
	private videoTimescale: number = 24000; // Default, updated from init segment
	private audioTimescale: number = 44100; // Default, updated from init segment
	private syncApplied = false; // True after offset is set

	constructor() {
		super();
		this.attachShadow({ mode: "open" });

		// Create styles
		const style = document.createElement("style");
		style.textContent = `
      :host {
        display: block;
        position: relative;
        background: #000;
      }
      video {
        width: 100%;
        height: auto;
        display: block;
      }
      .debug-overlay {
        position: absolute;
        top: 10px;
        left: 10px;
        background: rgba(0, 0, 0, 0.7);
        color: #0f0;
        font-family: monospace;
        font-size: 12px;
        padding: 8px;
        pointer-events: none;
        z-index: 100;
        border-radius: 4px;
      }
      .debug-overlay div {
        white-space: nowrap;
      }
    `;

		// Create video element
		this.video = document.createElement("video");
		this.video.controls = true;
		this.video.autoplay = true;
		this.video.muted = true; // Enable muted autoplay to bypass browser policies
		this.video.playsInline = true;

		this.video.addEventListener("play", () => {
			this.userPaused = false;
		});

		this.video.addEventListener("pause", () => {
			this.userPaused = true;
		});

		// Handle buffer underruns - try to resume when buffer refills
		this.video.addEventListener("waiting", () => {
			console.log("[MoqMsePlayer] Video waiting for data...");
		});

		this.video.addEventListener("stalled", () => {
			console.log("[MoqMsePlayer] Video stalled, attempting to recover...");
			// Try to seek to current position to unstick
			if (this.videoSourceBuffer && this.videoSourceBuffer.buffered.length > 0) {
				const bufferedEnd = this.videoSourceBuffer.buffered.end(0);
				if (bufferedEnd > this.video.currentTime + 0.5) {
					console.log("[MoqMsePlayer] Buffer ahead, seeking to resume");
					this.video.currentTime = this.video.currentTime + 0.1;
				}
			}
		});

		this.video.addEventListener("canplay", () => {
			if (!this.userPaused && this.video.paused) {
				console.log("[MoqMsePlayer] Can play, resuming...");
				this.video.play().catch((e) => console.error("[MoqMsePlayer] Resume failed:", e));
			}
		});

		const shadowRoot = this.shadowRoot;
		if (shadowRoot) {
			shadowRoot.appendChild(style);
			shadowRoot.appendChild(this.video);
		}
	}

	static get observedAttributes() {
		return ["url", "path", "debug", "latency"];
	}

	attributeChangedCallback(name: string, oldValue: string | null, newValue: string | null) {
		if (oldValue !== newValue && newValue !== null) {
			if (name === "url" || name === "path") {
				this.reconnect();
			}
		}
	}

	connectedCallback() {
		this.connect();
	}

	disconnectedCallback() {
		this.disconnect();
	}

	private reconnect() {
		if (this.client) {
			this.disconnect();
			this.connect();
		}
	}

	private async connect() {
		// Clear any existing status interval before creating a new one
		if (this.statusIntervalId !== null) {
			clearInterval(this.statusIntervalId);
			this.statusIntervalId = null;
		}

		// Start periodic status logging
		this.statusIntervalId = window.setInterval(() => {
			if (!this.video) return;
			const vBuf = this.videoSourceBuffer?.buffered.length
				? `${this.videoSourceBuffer.buffered.start(0).toFixed(2)}-${this.videoSourceBuffer.buffered.end(this.videoSourceBuffer.buffered.length - 1).toFixed(2)}`
				: "empty";
			const aBuf = this.audioSourceBuffer?.buffered.length
				? `${this.audioSourceBuffer.buffered.start(0).toFixed(2)}-${this.audioSourceBuffer.buffered.end(this.audioSourceBuffer.buffered.length - 1).toFixed(2)}`
				: "empty";
			console.log(
				`[MoqMsePlayer Status] Time: ${this.video.currentTime.toFixed(3)}s, State: ${this.video.readyState}, Paused: ${this.video.paused}, V-Buf: ${vBuf}, A-Buf: ${aBuf}`,
			);
		}, 1000);

		const url = this.getAttribute("url");
		const path = this.getAttribute("path");

		if (!url || !path) {
			console.error("[MoqMsePlayer] Missing url or path attributes");
			return;
		}

		try {
			// Initialize MediaSource but DON'T create SourceBuffer yet
			// Wait for catalog to get the correct codec
			await this.initMediaSource();

			// Connect to MoQ
			this.client = new MoqClient({
				relayUrl: url,
				broadcastName: path,
				onCatalog: (catalog) => this.handleCatalog(catalog),
				onData: (data, trackType) => this.handleData(data, trackType),
				onError: (error) => {
					console.error("[MoqMsePlayer] Error:", error);
					this.connectionState = "error";
					this.scheduleReconnect();
				},
				onConnected: () => {
					console.log("[MoqMsePlayer] Connected");
					this.connectionState = "connected";
				},
				onDisconnected: () => {
					console.log("[MoqMsePlayer] Disconnected");
					this.connectionState = "disconnected";
				},
			});

			this.connectionState = "connecting";
			await this.client.connect();

			// Start monitoring playback
			if (this.monitorInterval) clearInterval(this.monitorInterval);
			this.monitorInterval = window.setInterval(() => this.checkLiveEdge(), 1000);
		} catch (error) {
			console.error("[MoqMsePlayer] Connection failed:", error);
			this.scheduleReconnect();
		}
	}

	private scheduleReconnect() {
		if (this.reconnectTimer) return;
		console.log("[MoqMsePlayer] Scheduling reconnect in 2s...");
		this.reconnectTimer = window.setTimeout(() => {
			this.reconnectTimer = null;
			this.disconnect();
			this.connect();
		}, 2000);
	}

	private disconnect() {
		if (this.statusIntervalId !== null) {
			clearInterval(this.statusIntervalId);
			this.statusIntervalId = null;
		}
		if (this.monitorInterval) {
			clearInterval(this.monitorInterval);
			this.monitorInterval = null;
		}
		if (this.reconnectTimer) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = null;
		}
		if (this.client) {
			this.client.disconnect();
			this.client = null;
		}

		// Reset MediaSource and SourceBuffer
		if (this.mediaSource) {
			try {
				if (this.mediaSource.readyState === "open") {
					this.mediaSource.endOfStream();
				}
			} catch {
				// Ignore error if already closed
			}
			this.mediaSource = null;
		}
		this.videoSourceBuffer = null;
		this.audioSourceBuffer = null;
		this.videoQueue = [];
		this.audioQueue = [];
		this.isVideoProcessing = false;
		this.isAudioProcessing = false;
		this.catalogReceived = false;
		this.videoInitReceived = false;
		this.audioInitReceived = false;

		// Detach video source
		this.video.pause();
		this.video.removeAttribute("src");
		this.video.load();
	}

	private async initMediaSource(): Promise<void> {
		return new Promise((resolve) => {
			this.mediaSource = new MediaSource();
			this.video.src = URL.createObjectURL(this.mediaSource);

			this.mediaSource.addEventListener("sourceopen", () => {
				console.log("[MoqMsePlayer] MediaSource opened, waiting for catalog...");
				resolve();
			});
		});
	}

	private handleCatalog(catalog: Catalog.Root) {
		console.log("[MoqMsePlayer] Received catalog:", catalog);

		if (!this.mediaSource || this.mediaSource.readyState !== "open") {
			console.error("[MoqMsePlayer] MediaSource not open");
			return;
		}

		if (this.catalogReceived) {
			console.log("[MoqMsePlayer] Catalog already processed, skipping");
			return;
		}

		// Build codec string from catalog
		const videoCodec = this.getVideoCodec(catalog);
		const audioCodec = this.getAudioCodec(catalog);

		if (!videoCodec && !audioCodec) {
			console.error("[MoqMsePlayer] No codecs found in catalog");
			return;
		}

		// Build combined codec string for multiplexed fMP4
		const codecs: string[] = [];
		if (videoCodec) codecs.push(videoCodec);
		if (audioCodec) codecs.push(audioCodec);

		const mimeType = `video/mp4; codecs="${codecs.join(", ")}"`;
		console.log(
			"[MoqMsePlayer] Using codec from catalog:",
			mimeType,
			"- Supported:",
			MediaSource.isTypeSupported(mimeType),
		);

		if (!MediaSource.isTypeSupported(mimeType)) {
			console.error("[MoqMsePlayer] Codec not supported:", mimeType);
			return;
		}

		try {
			// Create separate SourceBuffers for video and audio
			const videoMimeType = `video/mp4; codecs="${videoCodec}"`;
			const audioMimeType = `audio/mp4; codecs="${audioCodec}"`;

			console.log("[MoqMsePlayer] Creating video SourceBuffer:", videoMimeType);
			this.videoSourceBuffer = this.mediaSource.addSourceBuffer(videoMimeType);
			this.videoSourceBuffer.mode = "sequence";
			this.videoSourceBuffer.addEventListener("updateend", () => {
				this.processVideoQueue();
				this.checkLiveEdge();
			});
			this.videoSourceBuffer.addEventListener("error", (e) => {
				console.error("[MoqMsePlayer] Video SourceBuffer error:", e);
			});

			console.log("[MoqMsePlayer] Creating audio SourceBuffer:", audioMimeType);
			this.audioSourceBuffer = this.mediaSource.addSourceBuffer(audioMimeType);
			this.audioSourceBuffer.mode = "sequence";
			this.audioSourceBuffer.addEventListener("updateend", () => {
				this.processAudioQueue();
			});
			this.audioSourceBuffer.addEventListener("error", (e) => {
				const sb = this.audioSourceBuffer;
				console.error("[MoqMsePlayer] Audio SourceBuffer error:", e, {
					updating: sb?.updating,
					mediaSourceState: this.mediaSource?.readyState,
				});
			});

			this.catalogReceived = true;
			console.log("[MoqMsePlayer] SourceBuffers created, waiting for init segments...");

			// Process any queued data
			this.processVideoQueue();
			this.processAudioQueue();
		} catch (error) {
			console.error("[MoqMsePlayer] Failed to create SourceBuffer:", error);
		}
	}

	private getVideoCodec(catalog: Catalog.Root): string | null {
		if (!catalog.video?.renditions) return null;

		const renditionKeys = Object.keys(catalog.video.renditions);
		if (renditionKeys.length === 0) return null;

		const firstRendition = catalog.video.renditions[renditionKeys[0]];
		return firstRendition?.codec || null;
	}

	private getAudioCodec(catalog: Catalog.Root): string | null {
		if (!catalog.audio?.renditions) return null;

		const renditionKeys = Object.keys(catalog.audio.renditions);
		if (renditionKeys.length === 0) return null;

		const firstRendition = catalog.audio.renditions[renditionKeys[0]];
		return firstRendition?.codec || null;
	}

	private log(msg: string, ...args: unknown[]) {
		if (this.hasAttribute("debug")) {
			console.log(msg, ...args);
		}
	}

	/**
	 * Strip the QUIC VarInt timestamp prefix from frame data.
	 * The hang server prepends a QUIC VarInt-encoded timestamp to each frame.
	 * QUIC VarInt format: first 2 bits indicate length (1, 2, 4, or 8 bytes).
	 * We need to remove this before passing to MSE.
	 */
	private stripTimestampPrefix(data: Uint8Array): Uint8Array {
		if (data.byteLength < 1) return data;

		// QUIC VarInt: first 2 bits of first byte indicate length
		const firstByte = data[0];
		const lengthBits = (firstByte >> 6) & 0x03;

		let varintLength: number;
		switch (lengthBits) {
			case 0:
				varintLength = 1;
				break; // 00 = 1 byte
			case 1:
				varintLength = 2;
				break; // 01 = 2 bytes
			case 2:
				varintLength = 4;
				break; // 10 = 4 bytes
			case 3:
				varintLength = 8;
				break; // 11 = 8 bytes
			default:
				varintLength = 1;
				break;
		}

		if (data.byteLength < varintLength) {
			console.error("[MoqMsePlayer] Data too short for QUIC VarInt");
			return data;
		}

		// Return the data without the timestamp prefix
		this.log(`[MoqMsePlayer] Stripped ${varintLength} byte QUIC VarInt timestamp`);
		return data.slice(varintLength);
	}

	/**
	 * Check if data is an init segment (starts with ftyp box)
	 */
	private isInitSegment(data: Uint8Array): boolean {
		if (data.byteLength < 8) return false;
		// Check for 'ftyp' fourcc at offset 4
		const fourcc = String.fromCharCode(data[4], data[5], data[6], data[7]);
		return fourcc === "ftyp";
	}

	/**
	 * Check if data is a media segment (starts with moof or styp)
	 */
	private isMediaSegment(data: Uint8Array): boolean {
		if (data.byteLength < 8) return false;
		const fourcc = String.fromCharCode(data[4], data[5], data[6], data[7]);
		return fourcc === "moof" || fourcc === "styp";
	}

	/**
	 * Debug helper: Log all MP4 boxes in a segment
	 */
	private logMp4Boxes(data: Uint8Array, trackType: string) {
		let offset = 0;
		const boxes: string[] = [];
		const dv = new DataView(data.buffer, data.byteOffset, data.byteLength);

		while (offset + 8 <= data.byteLength) {
			const boxSize = dv.getUint32(offset);
			const fourcc = String.fromCharCode(data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]);

			if (boxSize === 0) break; // End of boxes
			if (boxSize < 8 || offset + boxSize > data.byteLength) {
				boxes.push(`INVALID(size=${boxSize})`);
				break;
			}

			boxes.push(`${fourcc}(${boxSize})`);
			offset += boxSize;
		}

		console.log(`[MoqMsePlayer] ${trackType} init boxes:`, boxes.join(", "));

		// Log first 64 bytes as hex for debugging
		const hexBytes = Array.from(data.slice(0, Math.min(64, data.byteLength)))
			.map((b) => b.toString(16).padStart(2, "0"))
			.join(" ");
		console.log(`[MoqMsePlayer] ${trackType} init first 64 bytes:`, hexBytes);
	}

	/**
	 * Split a combined init+media segment into separate parts.
	 * The server sends ftyp + moov + moof + mdat combined.
	 * We need ftyp + moov as init segment, and moof + mdat as media segment.
	 */
	private splitInitAndMedia(data: Uint8Array): { initSegment: Uint8Array | null; mediaSegment: Uint8Array | null } {
		let offset = 0;
		let initEnd = 0;
		const dv = new DataView(data.buffer, data.byteOffset, data.byteLength);

		// Find the end of init segment (after ftyp and moov boxes)
		while (offset + 8 <= data.byteLength) {
			const boxSize = dv.getUint32(offset);
			const fourcc = String.fromCharCode(data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]);

			if (boxSize === 0 || boxSize < 8 || offset + boxSize > data.byteLength) {
				break;
			}

			// ftyp and moov are part of init segment
			if (fourcc === "ftyp" || fourcc === "moov") {
				initEnd = offset + boxSize;
				offset += boxSize;
			} else {
				// This is where media segment starts (moof, mdat, etc)
				break;
			}
		}

		const initSegment = initEnd > 0 ? data.slice(0, initEnd) : null;
		const mediaSegment = offset < data.byteLength ? data.slice(offset) : null;

		return { initSegment, mediaSegment };
	}

	private handleData(rawData: Uint8Array, trackType: "video" | "audio") {
		// Strip the varint timestamp prefix that hang adds to each frame
		const data = this.stripTimestampPrefix(rawData);

		// Log first 8 bytes to see what we're getting
		if (data.byteLength >= 8) {
			const fourcc = String.fromCharCode(data[4], data[5], data[6], data[7]);
			const size = new DataView(data.buffer, data.byteOffset, 4).getUint32(0);
			console.log(
				`[MoqMsePlayer] Received ${trackType} segment: fourcc='${fourcc}', size=${data.byteLength}, boxSize=${size}`,
			);
		}

		// Check if this is an init segment
		if (this.isInitSegment(data)) {
			let processInit = true; // Flag to pass to split logic
			if (trackType === "video") {
				if (this.videoInitReceived) {
					this.log(
						`[MoqMsePlayer] Skipping duplicate video INIT segment: ${data.byteLength} bytes. Continuing to check for media...`,
					);
					processInit = false; // Mark init as handled/skipped so we don't re-append it
				} else {
					console.log(`[MoqMsePlayer] Received VIDEO INIT segment: ${data.byteLength} bytes`);
					this.videoInitReceived = true;
				}
			} else {
				if (this.audioInitReceived) {
					this.log(
						`[MoqMsePlayer] Skipping duplicate audio INIT segment: ${data.byteLength} bytes. Continuing to check for media...`,
					);
					processInit = false;
				} else {
					console.log(`[MoqMsePlayer] Received AUDIO INIT segment: ${data.byteLength} bytes`);
					this.audioInitReceived = true;
				}
			}

			// Debug: Parse and log all boxes in the init segment
			this.logMp4Boxes(data, trackType);

			// The server sends ftyp + moov + moof + mdat combined!
			// We need to split this: only ftyp + moov should be the init segment
			// The moof + mdat should be queued as the first media segment
			const { initSegment, mediaSegment } = this.splitInitAndMedia(data);

			if (initSegment && processInit) {
				// Only process init if it wasn't marked as duplicate
				console.log(
					`[MoqMsePlayer] Extracted ${trackType} init segment (ftyp+moov): ${initSegment.byteLength} bytes`,
				);
				this.patchInitSegment(initSegment);
				if (trackType === "video") this.videoQueue.push({ data: initSegment, trackType });
				else this.audioQueue.push({ data: initSegment, trackType });
			}

			if (mediaSegment) {
				console.log(
					`[MoqMsePlayer] Extracted ${trackType} first media segment (moof+mdat): ${mediaSegment.byteLength} bytes`,
				);
				const timestamp = this.inspectMP4(mediaSegment, `${trackType} Media`);

				// Capture first timestamp for sync calculation
				if (trackType === "video" && this.firstVideoTimestamp === null && timestamp !== null) {
					this.firstVideoTimestamp = timestamp;
					console.log(`[Sync] First video timestamp: ${timestamp} (timescale: ${this.videoTimescale})`);
				}
				if (trackType === "audio" && this.firstAudioTimestamp === null && timestamp !== null) {
					this.firstAudioTimestamp = timestamp;
					console.log(`[Sync] First audio timestamp: ${timestamp} (timescale: ${this.audioTimescale})`);
				}

				if (trackType === "video") this.videoQueue.push({ data: mediaSegment, trackType });
				else this.audioQueue.push({ data: mediaSegment, trackType });
			}

			if (trackType === "video") this.processVideoQueue();
			else this.processAudioQueue();
			return;
		}

		// Inspect regular media segments and capture timestamp for sync
		const timestamp = this.inspectMP4(data, `${trackType} Media`);

		// Capture first timestamp for sync calculation (for non-bundled segments)
		if (trackType === "video" && this.firstVideoTimestamp === null && timestamp !== null) {
			this.firstVideoTimestamp = timestamp;
			console.log(`[Sync] First video timestamp: ${timestamp} (timescale: ${this.videoTimescale})`);
		}
		if (trackType === "audio" && this.firstAudioTimestamp === null && timestamp !== null) {
			this.firstAudioTimestamp = timestamp;
			console.log(`[Sync] First audio timestamp: ${timestamp} (timescale: ${this.audioTimescale})`);
		}

		// If we haven't received the init segment for this track type, drop its data
		if (trackType === "video" && !this.videoInitReceived) {
			this.log(`[MoqMsePlayer] Dropping video segment (waiting for video init)`);
			return;
		}
		if (trackType === "audio" && !this.audioInitReceived) {
			this.log(`[MoqMsePlayer] Dropping audio segment (waiting for audio init)`);
			return;
		}

		// Safety check: if queue is too large, drop old data
		if (this.videoQueue.length > 1000) {
			console.warn("[MoqMsePlayer] Video queue too large, clearing.");
			this.videoQueue = [];
		}
		if (this.audioQueue.length > 1000) {
			console.warn("[MoqMsePlayer] Audio queue too large, clearing.");
			this.audioQueue = [];
		}

		if (trackType === "video") {
			// console.log(`[MoqMsePlayer] Enqueuing video segment. Queue size: ${this.videoQueue.length+1}`);
			this.videoQueue.push({ data, trackType });
			this.processVideoQueue();
		} else {
			this.audioQueue.push({ data, trackType });
			this.processAudioQueue();
		}
	}

	private async processVideoQueue() {
		if (!this.videoSourceBuffer || !this.catalogReceived || !this.videoInitReceived) return;

		if (this.isVideoProcessing) {
			if (Date.now() - this.lastVideoProcessTime > 2000) {
				console.warn("[MoqMsePlayer] Video process loop stuck, forcing reset.");
				this.isVideoProcessing = false;
			} else {
				return;
			}
		}

		this.isVideoProcessing = true;
		this.lastVideoProcessTime = Date.now();

		try {
			while (this.videoQueue.length > 0) {
				const targetBuffer = this.videoSourceBuffer;

				// Wait if buffer is updating
				if (targetBuffer.updating) {
					await new Promise<void>((resolve) => {
						const onEnd = () => {
							targetBuffer.removeEventListener("updateend", onEnd);
							resolve();
						};
						targetBuffer.addEventListener("updateend", onEnd);
					});
				}

				if (this.videoQueue.length === 0) break;
				const item = this.videoQueue[0];

				try {
					const start = performance.now();
					await this.appendToBuffer(item.data, "video");
					const duration = performance.now() - start;
					if (duration > 50) {
						console.warn(`[MoqMsePlayer] Slow video append: ${duration.toFixed(1)}ms`);
					}
					this.videoQueue.shift(); // remove only after success
				} catch (e) {
					const error = e as Error;
					if (error.name === "QuotaExceededError") {
						console.warn("[MoqMsePlayer] Video QuotaExceededError. Cleaning up.");
						this.cleanupBuffer(true);
						// Don't shift, retry next time
						break;
					} else {
						console.error("[MoqMsePlayer] Error appending video:", error);
						this.videoQueue.shift(); // Drop problematic segment
					}
				}
			}
		} catch (e) {
			console.error("[MoqMsePlayer] Fatal error in processVideoQueue:", e);
		} finally {
			this.isVideoProcessing = false;
			// this.log("[MoqMsePlayer] Exiting processVideoQueue");
		}
	}

	private async processAudioQueue() {
		if (!this.audioSourceBuffer || !this.catalogReceived || !this.audioInitReceived) return;

		if (this.isAudioProcessing) {
			if (Date.now() - this.lastAudioProcessTime > 2000) {
				console.warn("[MoqMsePlayer] Audio process loop stuck, forcing reset.");
				this.isAudioProcessing = false;
			} else {
				return;
			}
		}

		this.isAudioProcessing = true;
		this.lastAudioProcessTime = Date.now();

		// SYNC GATE: Apply offset once when both first timestamps are available
		if (!this.syncApplied && this.firstVideoTimestamp !== null && this.firstAudioTimestamp !== null) {
			this.applySyncOffset();
		}

		try {
			while (this.audioQueue.length > 0) {
				const targetBuffer = this.audioSourceBuffer;

				// Wait if buffer is updating
				if (targetBuffer.updating) {
					await new Promise<void>((resolve) => {
						const onEnd = () => {
							targetBuffer.removeEventListener("updateend", onEnd);
							resolve();
						};
						targetBuffer.addEventListener("updateend", onEnd);
					});
				}

				if (this.audioQueue.length === 0) break;
				const item = this.audioQueue[0];

				try {
					await this.appendToBuffer(item.data, "audio");
					this.audioQueue.shift(); // remove only after success
				} catch (e) {
					const error = e as Error;
					if (error.name === "QuotaExceededError") {
						console.warn("[MoqMsePlayer] Audio QuotaExceededError. Cleaning up.");
						this.cleanupBuffer(true);
						// Don't shift, retry next time
						break;
					} else {
						console.error("[MoqMsePlayer] Error appending audio:", error);
						this.audioQueue.shift(); // Drop problematic segment
					}
				}
			}
		} catch (e) {
			console.error("[MoqMsePlayer] Fatal error in processAudioQueue:", e);
		} finally {
			this.isAudioProcessing = false;
		}
	}

	/**
	 * Calculate and apply the audio timestamp offset to synchronize with video.
	 * This is called once when both first video and audio timestamps are available.
	 */
	private applySyncOffset() {
		if (this.firstVideoTimestamp === null || this.firstAudioTimestamp === null) return;
		if (this.syncApplied) return;
		if (!this.audioSourceBuffer) return;

		// Convert both to seconds using their respective timescales
		const videoStartSec = this.firstVideoTimestamp / this.videoTimescale;
		const audioStartSec = this.firstAudioTimestamp / this.audioTimescale;

		// Calculate offset: we want audio to start when video starts
		const offset = videoStartSec - audioStartSec;

		console.log(
			`[Sync] Video starts at ${videoStartSec.toFixed(3)}s (${this.firstVideoTimestamp}/${this.videoTimescale})`,
		);
		console.log(
			`[Sync] Audio starts at ${audioStartSec.toFixed(3)}s (${this.firstAudioTimestamp}/${this.audioTimescale})`,
		);
		console.log(`[Sync] Calculated offset: ${offset.toFixed(3)}s (NOT applying - sequence mode starts both at 0)`);

		// In sequence mode, both tracks start at 0 regardless of original media timestamps.
		// Applying an offset would break sync by delaying one track.
		// The real fix for audio/video timestamp mismatch is on the server/encoder side.
		this.syncApplied = true;
	}

	private async appendToBuffer(data: Uint8Array, trackType: "video" | "audio"): Promise<void> {
		return new Promise((resolve, reject) => {
			const targetBuffer = trackType === "video" ? this.videoSourceBuffer : this.audioSourceBuffer;
			if (!targetBuffer) {
				reject(new Error(`No ${trackType} SourceBuffer`));
				return;
			}

			// Check if MediaSource is still open
			if (!this.mediaSource || this.mediaSource.readyState !== "open") {
				reject(new Error(`MediaSource not open (state: ${this.mediaSource?.readyState})`));
				return;
			}

			// Check if buffer is still updating
			if (targetBuffer.updating) {
				reject(new Error(`${trackType} SourceBuffer still updating`));
				return;
			}

			const isInit = this.isInitSegment(data);
			const isMedia = this.isMediaSegment(data);

			if (isInit) {
				console.log(`[MoqMsePlayer] Appending ${trackType} INIT segment, size:`, data.byteLength);
			} else if (isMedia) {
				this.log(`[MoqMsePlayer] Appending ${trackType} media segment, size:`, data.byteLength);
			} else {
				console.warn(
					`[MoqMsePlayer] Unknown ${trackType} segment type, first bytes:`,
					Array.from(data.slice(0, 16))
						.map((b) => b.toString(16).padStart(2, "0"))
						.join(" "),
				);
			}

			try {
				// Cast to BufferSource to handle TypeScript lib compatibility
				targetBuffer.appendBuffer(data as unknown as BufferSource);
			} catch (e: unknown) {
				const error = e as Error;
				console.error(`[MoqMsePlayer] ${trackType} appendBuffer failed:`, error.name, error.message);
				reject(error);
				return;
			}

			const onUpdateEnd = () => {
				targetBuffer.removeEventListener("updateend", onUpdateEnd);

				if (targetBuffer.buffered.length > 0) {
					const start = targetBuffer.buffered.start(0);
					const end = targetBuffer.buffered.end(targetBuffer.buffered.length - 1);
					// Diagnostic log - log frequently for now to debug "only 1 segment" issue
					console.log(
						`[MoqMsePlayer] ${trackType} UpdateEnd. Range: ${start.toFixed(3)}-${end.toFixed(3)} (Ranges: ${targetBuffer.buffered.length})`,
					);
				}
				resolve();
			};

			targetBuffer.addEventListener("updateend", onUpdateEnd);
		});
	}

	private cleanupBuffer(aggressive = false) {
		const currentTime = this.video.currentTime;
		const keepDuration = aggressive ? 5 : 60;

		// Cleanup both buffers
		for (const buffer of [this.videoSourceBuffer, this.audioSourceBuffer]) {
			if (!buffer || buffer.updating) continue;

			const buffered = buffer.buffered;
			for (let i = 0; i < buffered.length; i++) {
				const start = buffered.start(i);
				const end = buffered.end(i);

				if (end < currentTime - keepDuration) {
					this.log(`[MoqMsePlayer] Cleaning up buffer: ${start.toFixed(2)}-${end.toFixed(2)}`);
					buffer.remove(start, end);
					return;
				} else if (start < currentTime - keepDuration) {
					this.log(
						`[MoqMsePlayer] Cleaning up buffer: ${start.toFixed(2)}-${(currentTime - keepDuration).toFixed(2)}`,
					);
					buffer.remove(start, currentTime - keepDuration);
					return;
				}
			}
		}
	}

	private checkLiveEdge() {
		if (!this.videoSourceBuffer) return;
		try {
			const buffered = this.videoSourceBuffer?.buffered;
			if (!buffered || buffered.length === 0) {
				// this.log(`[MoqMsePlayer] Buffer empty. Time: ${this.video.currentTime.toFixed(2)}`);
				return;
			}

			// Log fragmentation if > 1 range
			if (buffered.length > 1) {
				const ranges = [];
				for (let i = 0; i < buffered.length; i++)
					ranges.push(`${buffered.start(i).toFixed(3)}-${buffered.end(i).toFixed(3)}`);
				console.warn(`[MoqMsePlayer] Fragmented Buffer: ${ranges.join(", ")}`);
			}

			const bufferedEnd = buffered.end(buffered.length - 1);
			const bufferedStart = buffered.start(0);
			const currentTime = this.video.currentTime;
			const paused = this.video.paused;

			if (paused) {
				if (this.userPaused) return;

				const bufferDuration = bufferedEnd - bufferedStart;
				this.log(
					`[MoqMsePlayer] Paused. Buffer: ${bufferDuration.toFixed(2)}s (${bufferedStart.toFixed(2)}-${bufferedEnd.toFixed(2)})`,
				);

				// Check if we need to start playing
				// If we are way behind the buffer start (e.g. at 0 while buffer is at 500), seek immediately
				const isOutOfSync = currentTime < bufferedStart - 0.5;

				// console.log(`[MoqMsePlayer] Autoplay Check: autoplay=${this.video.autoplay}, bufferDuration=${bufferDuration.toFixed(3)}, isOutOfSync=${isOutOfSync}, currentTime=${currentTime.toFixed(3)}, bufferedStart=${bufferedStart.toFixed(3)}`);

				// Relaxed condition: If out of sync, try to recover even if buffer is tiny (it might grow)
				if (this.video.autoplay && (bufferDuration > 0.1 || isOutOfSync)) {
					// Seek to live edge before playing
					if (currentTime < bufferedStart || currentTime > bufferedEnd) {
						const targetTime = Math.max(bufferedStart, bufferedEnd - 1.0);
						console.log(
							`[MoqMsePlayer] Seeking to live edge: ${targetTime.toFixed(2)}s (was at ${currentTime.toFixed(2)}s) range=${bufferedStart.toFixed(2)}-${bufferedEnd.toFixed(2)}`,
						);
						this.video.currentTime = targetTime;
					}
					this.log("[MoqMsePlayer] Attempting autoplay...");
					this.video.play().catch((e) => console.error("[MoqMsePlayer] Autoplay failed:", e));
				}
				return;
			}

			// HLS.js-style Live Sync
			const targetLatency = 2.0;
			const latency = bufferedEnd - currentTime;

			// Hard Seek if too far behind
			const isOutOfSync = currentTime < bufferedStart - 0.5;
			if (latency > 5.0 || isOutOfSync) {
				// Ensure we don't seek before the actual buffer start
				const safeTarget = Math.max(bufferedStart, bufferedEnd - targetLatency);

				this.log(
					`[MoqMsePlayer] Sync adjustment: latency=${latency.toFixed(2)}s, outOfSync=${isOutOfSync}, seeking to ${safeTarget.toFixed(2)}s coverage=${bufferedStart.toFixed(2)}-${bufferedEnd.toFixed(2)}`,
				);

				if (Math.abs(currentTime - safeTarget) > 0.5) {
					this.video.currentTime = safeTarget;
				}
				return;
			}

			// Catch-up via Playback Rate
			if (latency > 3.0) {
				if (this.video.playbackRate !== 1.1) {
					this.log(`[MoqMsePlayer] Catching up (latency ${latency.toFixed(2)}s), rate -> 1.1x`);
					this.video.playbackRate = 1.1;
				}
			} else if (latency < 2.0 && latency > -0.5) {
				if (this.video.playbackRate !== 1.0) {
					this.log(`[MoqMsePlayer] Latency good (${latency.toFixed(2)}s), rate -> 1.0x`);
					this.video.playbackRate = 1.0;
				}
			} else if (latency <= -0.5) {
				const targetPos = Math.max(0, bufferedEnd - 0.1);
				this.log(
					`[MoqMsePlayer] Ahead of buffer (${latency.toFixed(2)}s), seeking to ${targetPos.toFixed(2)}s`,
				);
				this.video.pause();
				this.video.currentTime = targetPos;
			}

			// Update Debug Overlay
			if (this.hasAttribute("debug")) {
				this.updateDebugOverlay(latency, bufferedStart, bufferedEnd);
			}

			// Regular buffer cleanup
			this.cleanupBuffer();
		} catch (_e) {
			// console.error("[MoqMsePlayer] checkLiveEdge error:", e);
		}
	}

	private updateDebugOverlay(latency: number, bufferedStart: number, bufferedEnd: number) {
		try {
			let debugEl = this.shadowRoot?.querySelector(".debug-overlay") as HTMLDivElement | null;
			if (!debugEl) {
				debugEl = document.createElement("div");
				debugEl.className = "debug-overlay";
				this.shadowRoot?.appendChild(debugEl);
			}

			const quality = this.video.getVideoPlaybackQuality?.() ?? null;
			const dropped = quality?.droppedVideoFrames ?? 0;
			const total = quality?.totalVideoFrames ?? 0;
			const vRanges = this.videoSourceBuffer?.buffered.length ?? 0;
			const aRanges = this.audioSourceBuffer?.buffered.length ?? 0;
			const vQ = this.videoQueue.length;
			const aQ = this.audioQueue.length;

			debugEl.innerHTML = `
        <div>Latency: ${latency.toFixed(3)}s</div>
        <div>Buffer: ${bufferedStart.toFixed(2)} - ${bufferedEnd.toFixed(2)}s (${(bufferedEnd - bufferedStart).toFixed(2)}s)</div>
        <div>Ranges: V=${vRanges} A=${aRanges}</div>
        <div>Rate: ${this.video.playbackRate.toFixed(2)}x</div>
        <div>Res: ${this.video.videoWidth}x${this.video.videoHeight}</div>
        <div>Time: ${this.video.currentTime.toFixed(2)}s</div>
        <div>State: ${this.connectionState}</div>
        <div>Queue: V=${vQ} A=${aQ}</div>
        <div>Init: V=${this.videoInitReceived ? "\u2713" : "..."} A=${this.audioInitReceived ? "\u2713" : "..."}</div>
        <div>Ready: ${this.video.readyState} | Net: ${this.video.networkState}</div>
        <div>Dropped: ${dropped} / ${total}</div>
        `;
		} catch (e) {
			console.error("Debug overlay error:", e);
		}
	}

	// --- MP4 Patching for Missing Durations ---

	private patchInitSegment(data: Uint8Array) {
		try {
			this.log(`[MP4 Patch] Scanning Init Segment (${data.byteLength} bytes)...`);
			const view = new DataView(data.buffer, data.byteOffset, data.byteLength);

			// Step 1: Map Track IDs to Types and Timescales
			const tracks = new Map<number, { type: string; timescale: number }>();

			// Find MOOV
			const moov = this.findBox(view, 0, "moov");
			if (!moov) {
				this.log("[MP4 Patch] No moov box found!");
				return;
			}

			// Iterate MOOV children to find TRAKs
			let offset = moov.start + 8;
			while (offset < moov.end) {
				const box = this.readBoxHeader(view, offset);
				if (box.size === 0) break;

				if (box.type === "trak") {
					const info = this.parseTrak(view, box.start, box.end);
					if (info) {
						tracks.set(info.id, info);
						this.log(`[MP4 Patch] Found Track ID=${info.id} Type=${info.type} Timescale=${info.timescale}`);
						// Store timescales for sync calculation
						if (info.type === "vide") this.videoTimescale = info.timescale;
						if (info.type === "soun") this.audioTimescale = info.timescale;
					}
				}
				offset += box.size;
			}

			// Step 2: Update TREX boxes in MVEX
			const mvex = this.findBox(view, moov.start + 8, "mvex", moov.end);
			if (mvex) {
				let inner = mvex.start + 8;
				while (inner < mvex.end) {
					const box = this.readBoxHeader(view, inner);
					if (box.type === "trex") {
						this.updateTrex(view, inner, tracks);
					}
					inner += box.size;
				}
			}
		} catch (e) {
			console.error("[MoqMsePlayer] Error patching init segment:", e);
		}
	}

	private findBox(
		view: DataView,
		start: number,
		type: string,
		max: number = -1,
	): { start: number; end: number; size: number } | null {
		let offset = start;
		const limit = max > 0 ? max : view.byteLength;
		while (offset + 8 <= limit) {
			const size = view.getUint32(offset);
			const name = String.fromCharCode(
				view.getUint8(offset + 4),
				view.getUint8(offset + 5),
				view.getUint8(offset + 6),
				view.getUint8(offset + 7),
			);
			if (name === type) {
				return { start: offset, end: offset + size, size };
			}
			if (size <= 0 || offset + size > limit) break; // Invalid

			// Container boxes to recurse? No, simple linear scan for now, caller handles hierarchy
			// If we receive a max, we assume we are scanning siblings
			offset += size;
		}
		return null;
	}

	private readBoxHeader(view: DataView, offset: number) {
		const size = view.getUint32(offset);
		const type = String.fromCharCode(
			view.getUint8(offset + 4),
			view.getUint8(offset + 5),
			view.getUint8(offset + 6),
			view.getUint8(offset + 7),
		);
		return { size, type, start: offset, end: offset + size };
	}

	private parseTrak(
		view: DataView,
		start: number,
		end: number,
	): { id: number; type: string; timescale: number } | null {
		let id = 0;
		let type = "unknown";
		let timescale = 0;

		// Scan children: tkhd, mdia
		const offset = start + 8;

		// Find TKHD
		const tkhd = this.findBox(view, offset, "tkhd", end);
		if (tkhd) {
			const version = view.getUint8(tkhd.start + 8);
			// Version(1)+Flags(3) = 4.
			// Creation(4/8) + Mod(4/8)
			let ptr = tkhd.start + 12;
			if (version === 1) ptr += 16;
			else ptr += 8;
			id = view.getUint32(ptr);
		}

		// Find MDIA
		const mdia = this.findBox(view, offset, "mdia", end);
		if (mdia) {
			// Find MDHD inside MDIA
			const mdhd = this.findBox(view, mdia.start + 8, "mdhd", mdia.end);
			if (mdhd) {
				const version = view.getUint8(mdhd.start + 8);
				let ptr = mdhd.start + 12;
				if (version === 1) ptr += 16;
				else ptr += 8; // Skip stamps
				timescale = view.getUint32(ptr);
			}
			// Find HDLR inside MDIA
			const hdlr = this.findBox(view, mdia.start + 8, "hdlr", mdia.end);
			if (hdlr) {
				// Ver(4), Pre(4), Type(4)
				const typeOffset = hdlr.start + 16;
				type = String.fromCharCode(
					view.getUint8(typeOffset),
					view.getUint8(typeOffset + 1),
					view.getUint8(typeOffset + 2),
					view.getUint8(typeOffset + 3),
				);
			}
		}

		if (id && timescale) return { id, type, timescale };
		return null;
	}

	private updateTrex(view: DataView, offset: number, tracks: Map<number, { type: string; timescale: number }>) {
		// trek: Size(4), Type(4), VerFlags(4), TrackID(4), DefDescIdx(4), DefDur(4)...
		const trackId = view.getUint32(offset + 12);
		const info = tracks.get(trackId);

		const currentDuration = view.getUint32(offset + 20);

		if (info && currentDuration === 0) {
			let newDuration = 0;
			if (info.type === "vide") {
				// Assume 30fps if unknown
				newDuration = Math.floor(info.timescale / 30);
			} else if (info.type === "soun") {
				// AAC default is 1024 samples
				newDuration = 1024;
			}

			if (newDuration > 0) {
				this.log(
					`[MP4 Patch] Overwriting TREX duration for Track ${trackId} (${info.type}). Old=${currentDuration}, New=${newDuration}`,
				);
				view.setUint32(offset + 20, newDuration); // Big Endian default
			}
		} else {
			// this.log(`[MP4 Patch] TREX Track ${trackId} has duration ${currentDuration} (Type: ${info?.type})`);
		}
	}

	// Debug: Inspect MP4 atoms to find timescale/duration issues
	private inspectLogCount = 0;

	private inspectMP4(data: Uint8Array, label: string): number | null {
		// Rate limit logging, but ALWAYS parse audio if we need its first timestamp for sync
		const isAudio = label.includes("audio");
		const needAudioTimestamp = isAudio && this.firstAudioTimestamp === null;

		if (this.inspectLogCount > 10 && label.includes("Media") && !label.includes("video") && !needAudioTimestamp) {
			return null; // Limit spam, but allow video and first audio timestamp capture
		}
		if (label.includes("Media")) this.inspectLogCount++;

		let foundTimestamp: number | null = null;

		try {
			let offset = 0;
			const view = new DataView(data.buffer, data.byteOffset, data.byteLength);

			this.log(`[MP4 Debug] Inspecting ${label} (${data.byteLength} bytes)`);

			let iter = 0;
			while (offset < data.byteLength && iter < 50) {
				iter++;
				if (offset + 8 > data.byteLength) break;
				const size = view.getUint32(offset);
				const type = String.fromCharCode(
					view.getUint8(offset + 4),
					view.getUint8(offset + 5),
					view.getUint8(offset + 6),
					view.getUint8(offset + 7),
				);

				const boxSize = size === 0 ? data.byteLength - offset : size;
				this.log(`  Box: ${type}, Size: ${boxSize} (Offset: ${offset})`);

				if (type === "mdhd") {
					this.parseMdhd(view, offset + 8);
				}
				if (type === "trun") {
					this.parseTrun(view, offset + 8);
				}
				if (type === "tfdt") {
					foundTimestamp = this.parseTfdt(view, offset + 8);
				}

				if (["moov", "trak", "mdia", "minf", "stbl", "mvex", "moof", "traf"].includes(type)) {
					offset += 8; // Enter container
				} else {
					offset += boxSize; // Skip leaf
				}
			}
		} catch (e) {
			this.log(`[MP4 Debug] Error inspecting: ${e}`);
		}

		return foundTimestamp;
	}

	private parseTfdt(view: DataView, offset: number): number {
		// Version(1), Flags(3), BaseMediaDecodeTime(4 or 8)
		const version = view.getUint8(offset);
		let baseMediaDecodeTime = 0;
		if (version === 1) {
			// 64-bit: high 32, low 32. JS only supports reading 32 safely or BigInt.
			// Let's assume high 32 is 0 or use BigInt if possible, but logging simple number
			const high = view.getUint32(offset + 4);
			const low = view.getUint32(offset + 8);
			// If high is > 0 this might be inaccurate as number, but fine for debug logs usually
			baseMediaDecodeTime = high * 4294967296 + low;
			this.log(`    [tfdt] BaseMediaDecodeTime (64-bit): ${baseMediaDecodeTime}`);
		} else {
			baseMediaDecodeTime = view.getUint32(offset + 4);
			this.log(`    [tfdt] BaseMediaDecodeTime (32-bit): ${baseMediaDecodeTime}`);
		}
		return baseMediaDecodeTime;
	}

	private parseMdhd(view: DataView, offset: number) {
		if (offset + 24 > view.byteLength) return;
		const version = view.getUint8(offset);
		let ptr = offset + 4;
		if (version === 1) {
			ptr += 16;
		} else {
			ptr += 8;
		}
		if (ptr + 4 <= view.byteLength) {
			const timescale = view.getUint32(ptr);
			this.log(`    [mdhd] Version: ${version}, Timescale: ${timescale}`);
		}
	}

	private parseTrun(view: DataView, offset: number) {
		if (offset + 12 > view.byteLength) return;
		const fullFlags = view.getUint32(offset);
		const flags = fullFlags & 0xffffff;
		const sampleCount = view.getUint32(offset + 4);

		const durationPresent = (flags & 0x100) !== 0;

		this.log(`    [trun] SampleCount: ${sampleCount}, DurationPresent: ${durationPresent}`);

		if (durationPresent && sampleCount > 0) {
			let ptr = offset + 8;
			if (flags & 0x01) ptr += 4;
			if (flags & 0x04) ptr += 4;

			if (ptr + 4 <= view.byteLength) {
				const firstDuration = view.getUint32(ptr);
				this.log(`    [trun] First Sample Duration: ${firstDuration}`);
			}
		} else {
			this.log(`    [trun] No duration present in samples.`);
		}
	}
}

// Register the custom element
customElements.define("moq-mse-player", MoqMsePlayer);

declare global {
	interface HTMLElementTagNameMap {
		"moq-mse-player": MoqMsePlayer;
	}
}
