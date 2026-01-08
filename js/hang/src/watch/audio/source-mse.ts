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

	#appendQueue: Uint8Array[] = [];
	static readonly MAX_QUEUE_SIZE = 10;

	#stats = new Signal<AudioStats | undefined>(undefined);
	readonly stats = this.#stats;

	readonly latency: Signal<Time.Milli>;

	#signals = new Effect();

	constructor(latency: Signal<Time.Milli>) {
		this.latency = latency;
	}

	async initialize(config: Catalog.AudioConfig): Promise<void> {
		const mimeType = Mime.buildAudioMimeType(config);
		if (!mimeType) {
			throw new Error(`Unsupported codec for MSE: ${config.codec}`);
		}

		this.#audio = document.createElement("audio");
		this.#audio.style.display = "none";
		this.#audio.muted = false; // Allow audio playback
		this.#audio.volume = 1.0; // Set initial volume to 1.0
		document.body.appendChild(this.#audio);

		this.#audioElement.set(this.#audio);

		this.#mediaSource = new MediaSource();
		const url = URL.createObjectURL(this.#mediaSource);
		this.#audio.src = url;
		this.#audio.currentTime = 0;

		await new Promise<void>((resolve, reject) => {
			const timeout = setTimeout(() => {
				reject(new Error("MediaSource sourceopen timeout"));
			}, 5000);

			this.#mediaSource!.addEventListener(
				"sourceopen",
				() => {
					clearTimeout(timeout);
					try {
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

		this.#sourceBuffer.addEventListener("updateend", () => {
			this.#processAppendQueue();

		});

		this.#sourceBuffer.addEventListener("error", (e) => {
			console.error("SourceBuffer error:", e);
		});
	}

	async appendFragment(fragment: Uint8Array): Promise<void> {
		if (!this.#sourceBuffer || !this.#mediaSource) {
			throw new Error("SourceBuffer not initialized");
		}

		// Don't queue fragments if MediaSource is closed
		if (this.#mediaSource.readyState === "closed") {
			return;
		}

		if (this.#appendQueue.length >= SourceMSE.MAX_QUEUE_SIZE) {
			const discarded = this.#appendQueue.shift();
			console.warn(`[MSE Audio] Queue full (${SourceMSE.MAX_QUEUE_SIZE}), discarding oldest fragment (${discarded?.byteLength ?? 0} bytes)`);
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
			return;
		}

		const fragment = this.#appendQueue.shift()!;

		try {
			// appendBuffer accepts BufferSource (ArrayBuffer or ArrayBufferView)
			this.#sourceBuffer.appendBuffer(fragment as BufferSource);

			this.#stats.update((current) => ({
				bytesReceived: (current?.bytesReceived ?? 0) + fragment.byteLength,
			}));
		} catch (error) {
			console.error("[MSE Audio] Error appending fragment:", error);
		}
	}

	async runTrack(
		effect: Effect,
		broadcast: Moq.Broadcast,
		name: string,
		config: Catalog.AudioConfig,
		catalog: Catalog.Audio,
	): Promise<void> {
		await this.initialize(config);

		const sub = broadcast.subscribe(name, catalog.priority);
		effect.cleanup(() => sub.close());

		const consumer = new Frame.Consumer(sub, {
			latency: this.latency,
			container: "fmp4", // CMAF fragments
		});
		effect.cleanup(() => consumer.close());

		effect.spawn(async () => {
			if (!this.#audio) return;

			await new Promise<void>((resolve) => {
				let checkCount = 0;
				const maxChecks = 100; // 10 seconds max wait

				let hasSeeked = false;
				const checkReady = () => {
					checkCount++;
					if (this.#audio && this.#sourceBuffer) {
						const audioBuffered = this.#audio.buffered;
						const hasBufferedData = this.#sourceBuffer.buffered.length > 0;

						if (hasBufferedData && audioBuffered && audioBuffered.length > 0 && !hasSeeked) {
							const currentTime = this.#audio.currentTime;
							let isTimeBuffered = false;
							for (let i = 0; i < audioBuffered.length; i++) {
								if (audioBuffered.start(i) <= currentTime && currentTime < audioBuffered.end(i)) {
									isTimeBuffered = true;
									break;
								}
							}
							if (!isTimeBuffered) {
								const seekTime = audioBuffered.start(0);
								this.#audio.currentTime = seekTime;
								hasSeeked = true;
								setTimeout(checkReady, 100);
								return;
							}
						}

						// Try to play if we have buffered data, even if readyState is low
						// The browser will start playing when it's ready
						if (hasBufferedData && this.#audio.readyState >= HTMLMediaElement.HAVE_METADATA) {
							this.#audio.play().then(() => {
								resolve();
							}).catch((error) => {
								console.error("[MSE Audio] Audio play() failed (initial):", error);
								if (checkCount < maxChecks) {
									setTimeout(checkReady, 200);
								} else {
									resolve();
								}
							});
						} else if (checkCount >= maxChecks) {
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

		let initSegmentReceived = false;

		// Helper function to detect moov atom in the buffer
		// This searches for "moov" atom at any position, not just at the start
		// The init segment may have other atoms before "moov" (e.g., "ftyp")
		function hasMoovAtom(data: Uint8Array): boolean {
			let offset = 0;
			const len = data.length;

			while (offset + 8 <= len) {
				// Atom size (big endian)
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

				// Avoid infinite loops if size is broken
				if (size < 8) break;
				offset += size;
			}

			return false;
		}

		// Read fragments and append to SourceBuffer
		// We group fragments by MOQ group before appending
		effect.spawn(async () => {
			let frameCount = 0;
			let currentGroup: number | undefined = undefined;
			let groupFragments: Uint8Array[] = []; // Accumulate fragments for current group

			for (; ;) {
				const frame = await Promise.race([consumer.decode(), effect.cancel]);
				if (!frame) {
					if (groupFragments.length > 0 && initSegmentReceived && this.#mediaSource?.readyState === "open") {
						const groupData = this.#concatenateFragments(groupFragments);
						await this.appendFragment(groupData);
						groupFragments = [];
					}
					break;
				}

				// Stop processing if MediaSource is closed
				if (this.#mediaSource?.readyState === "closed") {
					break;
				}

				frameCount++;

				const isMoovAtom = hasMoovAtom(frame.data);
				const isInitSegment = isMoovAtom && !initSegmentReceived;

				if (isInitSegment) {
					if (groupFragments.length > 0 && initSegmentReceived && this.#mediaSource?.readyState === "open") {
						const groupData = this.#concatenateFragments(groupFragments);
						await this.appendFragment(groupData);
						groupFragments = [];
					}

					await this.appendFragment(frame.data);
					initSegmentReceived = true;
					continue;
				}

				if (!initSegmentReceived) {
					continue;
				}

				if (currentGroup !== undefined && frame.group !== currentGroup) {
					if (groupFragments.length > 0 && this.#mediaSource?.readyState === "open") {
						const groupData = this.#concatenateFragments(groupFragments);
						await this.appendFragment(groupData);
						groupFragments = [];
					}
				}

				if (currentGroup === undefined || frame.group !== currentGroup) {
					currentGroup = frame.group;
					groupFragments = [];
				}

				groupFragments.push(frame.data);

				// Append immediately for low latency audio sync
				if (groupFragments.length >= 1 && this.#mediaSource?.readyState === "open") {
					const groupData = this.#concatenateFragments(groupFragments);
					await this.appendFragment(groupData);
					groupFragments = [];
				}
			}
		});
	}

	close(): void {
		this.#appendQueue = [];

		this.#audioElement.set(undefined);

		if (this.#sourceBuffer && this.#mediaSource) {
			try {
				if (this.#sourceBuffer.updating) {
					this.#sourceBuffer.abort();
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
				URL.revokeObjectURL(this.#audio?.src || "");
			} catch (error) {
				console.error("Error closing MediaSource:", error);
			}
		}

		if (this.#audio) {
			this.#audio.pause();
			this.#audio.src = "";
			this.#audio.remove();
		}

		this.#signals.close();
	}
}

