import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../../catalog";
import * as Frame from "../../frame";
import type * as Time from "../../time";
import * as Mime from "../../util/mime";

export interface AudioStats {
	bytesReceived: number;
}

/**
 * MSE-based audio source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 * The browser handles decoding and playback directly from the HTMLAudioElement.
 */
export class SourceMSE {
	#audio?: HTMLAudioElement;
	#mediaSource?: MediaSource;
	#sourceBuffer?: SourceBuffer;
	
	// Signal to expose audio element for volume/mute control
	#audioElement = new Signal<HTMLAudioElement | undefined>(undefined);
	readonly audioElement = this.#audioElement as Getter<HTMLAudioElement | undefined>;

	// Cola de fragmentos esperando ser añadidos
	// Límite máximo para evitar crecimiento infinito en live streaming
	#appendQueue: Uint8Array[] = [];
	static readonly MAX_QUEUE_SIZE = 10; // Máximo de fragmentos en cola

	#stats = new Signal<AudioStats | undefined>(undefined);
	readonly stats = this.#stats;

	readonly latency: Signal<Time.Milli>;

	#signals = new Effect();

	constructor(latency: Signal<Time.Milli>) {
		this.latency = latency;
	}

	async initialize(config: Catalog.AudioConfig): Promise<void> {
		// Build MIME type from codec
		const mimeType = Mime.buildAudioMimeType(config);
		if (!mimeType) {
			throw new Error(`Unsupported codec for MSE: ${config.codec}`);
		}

		// Create hidden audio element
		this.#audio = document.createElement("audio");
		this.#audio.style.display = "none";
		this.#audio.muted = false; // Allow audio playback
		this.#audio.volume = 1.0; // Set initial volume to 1.0
		document.body.appendChild(this.#audio);
		
		console.log("[MSE Audio] Audio element created:", {
			muted: this.#audio.muted,
			volume: this.#audio.volume,
			readyState: this.#audio.readyState,
		});
		
		// Don't auto-play here - let Emitter control play/pause state
		// The initial play() call is handled in runTrack() after data is buffered
		
		// Expose audio element via Signal for Emitter to control volume/mute
		this.#audioElement.set(this.#audio);

		// Create MediaSource
		this.#mediaSource = new MediaSource();
		const url = URL.createObjectURL(this.#mediaSource);
		this.#audio.src = url;

		// Set initial time to 0 to ensure playback starts from the beginning
		this.#audio.currentTime = 0;

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
	}

	#setupSourceBuffer(): void {
		if (!this.#sourceBuffer) return;

		// Procesar la cola cuando termine la operación actual
		this.#sourceBuffer.addEventListener("updateend", () => {
			this.#processAppendQueue();
			// Don't auto-resume here - let Emitter control play/pause state
		});

		this.#sourceBuffer.addEventListener("error", (e) => {
			console.error("SourceBuffer error:", e);
		});
	}

	async appendFragment(fragment: Uint8Array): Promise<void> {
		if (!this.#sourceBuffer || !this.#mediaSource) {
			throw new Error("SourceBuffer not initialized");
		}

		// Si la cola está llena, descartar el fragmento más antiguo (FIFO)
		// Esto mantiene baja la latencia en live streaming
		if (this.#appendQueue.length >= SourceMSE.MAX_QUEUE_SIZE) {
			const discarded = this.#appendQueue.shift();
			console.warn(`[MSE Audio] Queue full (${SourceMSE.MAX_QUEUE_SIZE}), discarding oldest fragment (${discarded?.byteLength ?? 0} bytes)`);
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
			console.error(`[MSE Audio] MediaSource not open: ${this.#mediaSource?.readyState}`);
			return;
		}

		const fragment = this.#appendQueue.shift()!;
		
		try {
			// appendBuffer accepts BufferSource (ArrayBuffer or ArrayBufferView)
			this.#sourceBuffer.appendBuffer(fragment as BufferSource);
			
			// Update stats
			this.#stats.update((current) => ({
				bytesReceived: (current?.bytesReceived ?? 0) + fragment.byteLength,
			}));
		} catch (error) {
			console.error("[MSE Audio] Error appending fragment:", error);
			// No reintentamos - el fragmento se descarta
		}
	}

	async runTrack(
		effect: Effect,
		broadcast: Moq.Broadcast,
		name: string,
		config: Catalog.AudioConfig,
	): Promise<void> {
		// Initialize MSE
		await this.initialize(config);

		const catalog = { priority: 128 }; // TODO: Get from actual catalog
		const sub = broadcast.subscribe(name, catalog.priority);
		effect.cleanup(() => sub.close());

		// Create consumer for CMAF fragments
		const consumer = new Frame.Consumer(sub, {
			latency: this.latency,
			container: "fmp4", // CMAF fragments
		});
		effect.cleanup(() => consumer.close());

		console.log("[MSE Audio] Consumer created, waiting for frames...");

		// Initial play attempt when we have data buffered
		// After this, Emitter controls play/pause state
		effect.spawn(async () => {
			if (!this.#audio) return;

			// Wait for some data to be buffered, then attempt to play
			await new Promise<void>((resolve) => {
				let checkCount = 0;
				const maxChecks = 100; // 10 seconds max wait
				
				let hasSeeked = false;
				const checkReady = () => {
					checkCount++;
					if (this.#audio && this.#sourceBuffer) {
						const bufferedRanges = this.#sourceBuffer.buffered;
						const audioBuffered = this.#audio.buffered;
						const hasBufferedData = bufferedRanges.length > 0;
						const bufferedInfo = hasBufferedData
							? `${bufferedRanges.length} ranges, last: ${bufferedRanges.start(bufferedRanges.length - 1).toFixed(2)}-${bufferedRanges.end(bufferedRanges.length - 1).toFixed(2)}`
							: "no ranges";
						console.log(`[MSE Audio] Audio readyState: ${this.#audio.readyState}, buffered: ${bufferedInfo}, checkCount: ${checkCount}`);
						
						// Check if currentTime is within buffered range
						if (hasBufferedData && audioBuffered && audioBuffered.length > 0 && !hasSeeked) {
							const currentTime = this.#audio.currentTime;
							const isTimeBuffered = audioBuffered.start(0) <= currentTime && currentTime < audioBuffered.end(audioBuffered.length - 1);
							
							// If we have buffered data but current time is not in range, seek immediately
							if (!isTimeBuffered) {
								const seekTime = audioBuffered.start(0);
								console.log(`[MSE Audio] Seeking to buffered start time: ${seekTime.toFixed(3)} (currentTime=${currentTime.toFixed(3)})`);
								this.#audio.currentTime = seekTime;
								hasSeeked = true;
								// Continue checking after seek
								setTimeout(checkReady, 100);
								return;
							}
						}
						
						// Try to play if we have buffered data, even if readyState is low
						// The browser will start playing when it's ready
						if (hasBufferedData && this.#audio.readyState >= HTMLMediaElement.HAVE_METADATA) {
							console.log("[MSE Audio] Audio has buffered data, attempting initial play...", {
								readyState: this.#audio.readyState,
								muted: this.#audio.muted,
								volume: this.#audio.volume,
								paused: this.#audio.paused,
								hasBufferedData,
								currentTime: this.#audio.currentTime,
							});
							this.#audio.play().then(() => {
								console.log("[MSE Audio] Audio play() succeeded (initial)");
								resolve();
							}).catch((error) => {
								console.error("[MSE Audio] Audio play() failed (initial):", error);
								// Continue checking, might succeed later
								if (checkCount < maxChecks) {
									setTimeout(checkReady, 200);
								} else {
									resolve();
								}
							});
						} else if (checkCount >= maxChecks) {
							console.warn("[MSE Audio] Audio did not get buffered data after 10 seconds");
							resolve();
						} else {
							setTimeout(checkReady, 100);
						}
					} else if (checkCount >= maxChecks) {
						resolve();
					} else {
						setTimeout(checkReady, 100);
					}
				};
				checkReady();
			});
		});

		// Track if we've received the init segment (moov)
		let initSegmentReceived = false;

		// Helper function to detect moov atom in the buffer
		// This searches for "moov" atom at any position, not just at the start
		// The init segment may have other atoms before "moov" (e.g., "ftyp")
		function hasMoovAtom(data: Uint8Array): boolean {
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

				if (type === "moov") return true;

				// Evitar loops infinitos si el size viene roto
				if (size < 8) break;
				offset += size;
			}

			return false;
		}

		// Read fragments and append to SourceBuffer
		// MSE works better when appending complete groups (GOPs for video, sample groups for audio)
		// We group fragments by MOQ group before appending
		effect.spawn(async () => {
			let frameCount = 0;
			let currentGroup: number | undefined = undefined;
			let groupFragments: Uint8Array[] = []; // Accumulate fragments for current group

			console.log("[MSE Audio] Starting to read frames from consumer...");

			for (;;) {
				const frame = await Promise.race([consumer.decode(), effect.cancel]);
				if (!frame) {
					// Append any remaining group fragments before finishing
					if (groupFragments.length > 0 && initSegmentReceived) {
						const groupData = this.#concatenateFragments(groupFragments);
						console.log(`[MSE Audio] Appending final group (group ${currentGroup}): ${groupFragments.length} fragments, total size=${groupData.byteLength}`);
						await this.appendFragment(groupData);
						groupFragments = [];
					}
					console.log(`[MSE Audio] No more frames, total frames processed: ${frameCount}`);
					break;
				}
				frameCount++;
				console.log(`[MSE Audio] Received frame ${frameCount}: timestamp=${frame.timestamp}, size=${frame.data.byteLength}, group=${frame.group}, keyframe=${frame.keyframe}`);

				// Check if this is the init segment (moov)
				const isMoovAtom = hasMoovAtom(frame.data);
				const isInitSegment = isMoovAtom && !initSegmentReceived;
				
				if (isInitSegment) {
					// Append any pending group before processing init segment
					if (groupFragments.length > 0 && initSegmentReceived) {
						const groupData = this.#concatenateFragments(groupFragments);
						console.log(`[MSE Audio] Appending group (group ${currentGroup}) before init segment: ${groupFragments.length} fragments`);
						await this.appendFragment(groupData);
						groupFragments = [];
					}

					// This is the init segment (moov), append it first
					await this.appendFragment(frame.data);
					initSegmentReceived = true;
					console.log("[MSE Audio] Init segment (moov) received and appended");
					continue;
				}

				// This is a regular fragment (moof+mdat)
				if (!initSegmentReceived) {
					console.warn("[MSE Audio] Received fragment before init segment, skipping");
					continue;
				}

				// Check if we're starting a new group
				if (currentGroup !== undefined && frame.group !== currentGroup) {
					// Append the complete group from previous group
					if (groupFragments.length > 0) {
						const groupData = this.#concatenateFragments(groupFragments);
						console.log(`[MSE Audio] Appending complete group (group ${currentGroup}): ${groupFragments.length} fragments, total size=${groupData.byteLength}`);
						await this.appendFragment(groupData);
						groupFragments = [];
					}
				}

				// If this is the first fragment of a new group, start accumulating
				if (currentGroup === undefined || frame.group !== currentGroup) {
					currentGroup = frame.group;
					groupFragments = [];
				}

				groupFragments.push(frame.data);
				console.log(`[MSE Audio] Accumulating fragment for group (group ${frame.group}): timestamp=${frame.timestamp}, size=${frame.data.byteLength}, total fragments in group=${groupFragments.length}`);

				// For live streaming: append immediately if we have at least one fragment
				// This ensures we don't wait indefinitely for more fragments
				// We'll still group by MOQ group, but append more aggressively
				if (groupFragments.length >= 1) {
					// Append immediately - MSE can handle single fragments if they're complete
					const groupData = this.#concatenateFragments(groupFragments);
					console.log(`[MSE Audio] Appending group immediately (group ${currentGroup}): ${groupFragments.length} fragments, total size=${groupData.byteLength}`);
					await this.appendFragment(groupData);
					groupFragments = [];
				}
			}
		});
	}

	close(): void {
		// Limpiar la cola
		this.#appendQueue = [];
		
		// Clear audio element Signal
		this.#audioElement.set(undefined);

		// Clean up SourceBuffer
		if (this.#sourceBuffer && this.#mediaSource) {
			try {
				if (this.#sourceBuffer.updating) {
					this.#sourceBuffer.abort();
				}
				// Don't call endOfStream() here - let it be called only once for MediaSource
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
				URL.revokeObjectURL(this.#audio?.src || "");
			} catch (error) {
				console.error("Error closing MediaSource:", error);
			}
		}

		// Remove audio element
		if (this.#audio) {
			this.#audio.pause();
			this.#audio.src = "";
			this.#audio.remove();
		}

		this.#signals.close();
	}
}

