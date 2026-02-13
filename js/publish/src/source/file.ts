import { Effect, Signal } from "@moq/signals";
import type { StreamTrack as AudioStreamTrack } from "../audio/types";
import type { StreamTrack as VideoStreamTrack } from "../video/types";

export interface FileSourceConfig {
	enabled?: boolean | Signal<boolean>;
	file?: globalThis.File | Signal<globalThis.File | undefined>;
}

export class File {
	file = new Signal<globalThis.File | undefined>(undefined);
	signals = new Effect();

	source = new Signal<{ video?: VideoStreamTrack; audio?: AudioStreamTrack }>({});
	enabled: Signal<boolean>;

	constructor(config: FileSourceConfig) {
		this.enabled = Signal.from(config.enabled ?? false);
		this.file = Signal.from(config.file);

		this.signals.effect((effect) => {
			const values = effect.getAll([this.file, this.enabled]);
			if (!values) return;
			const [file] = values;

			this.#decode(file, effect).catch((err) => {
				console.error("Failed to decode file:", err);
			});
		});
	}

	async #decode(file: globalThis.File, effect: Effect) {
		const type = file.type;

		if (type.startsWith("image/")) {
			await this.#decodeImage(file, effect);
		} else if (type.startsWith("video/") || type.startsWith("audio/")) {
			await this.#decodeMedia(file, effect);
		} else {
			throw new Error(`Unsupported file type: ${type}`);
		}
	}

	async #decodeImage(file: globalThis.File, effect: Effect) {
		const img = new Image();
		const url = URL.createObjectURL(file);
		img.src = url;
		await img.decode();

		effect.cleanup(() => URL.revokeObjectURL(url));

		const canvas = document.createElement("canvas");
		canvas.width = img.width;
		canvas.height = img.height;
		const ctx = canvas.getContext("2d");

		if (!ctx) {
			throw new Error("Failed to create 2D canvas context");
		}

		const interval = setInterval(() => {
			ctx.drawImage(img, 0, 0);
		}, 1000 / 30);

		effect.cleanup(() => clearInterval(interval));

		const stream = canvas.captureStream(30);
		const videoTrack = stream.getVideoTracks()[0];

		if (!videoTrack) {
			throw new Error("Failed to capture video track from canvas stream");
		}

		effect.set(this.source, { video: videoTrack as VideoStreamTrack }, {});
	}

	async #decodeMedia(file: globalThis.File, effect: Effect) {
		const video = document.createElement("video") as HTMLVideoElement & {
			captureStream(): MediaStream;
		};

		const url = URL.createObjectURL(file);
		video.src = url;
		video.loop = true;
		video.muted = true;

		await new Promise<void>((resolve, reject) => {
			video.onloadedmetadata = () => resolve();
			video.onerror = () => reject(new Error("Failed to load video"));
		});

		await video.play();

		effect.cleanup(() => {
			video.pause();
			URL.revokeObjectURL(url);
		});

		const stream = video.captureStream();
		const videoTrack = stream.getVideoTracks()[0];
		const audioTrack = stream.getAudioTracks()[0];

		if (!videoTrack && !audioTrack) {
			throw new Error("Failed to capture any tracks from video element");
		}
		effect.set(
			this.source,
			{
				video: videoTrack as VideoStreamTrack,
				audio: audioTrack as AudioStreamTrack,
			},
			{},
		);
	}

	close() {
		this.signals.close();
	}
}
