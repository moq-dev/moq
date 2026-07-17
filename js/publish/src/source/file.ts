import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Audio from "../audio";
import type { StreamTrack as VideoStreamTrack } from "../video/types";

// Signals the file source reads.
export type FileInput = {
	// Whether to decode the file. When false the tracks are stopped and `out.source` clears.
	enabled: Getter<boolean>;
};

/** Constructor options: the wired inputs plus the file to decode. */
export interface FileProps extends Inputs<FileInput> {
	/** Seed the file; also live-editable via `file.file` and {@link File.prompt}. */
	file?: globalThis.File | Signal<globalThis.File | undefined>;
}

type FileOutput = {
	// The tracks decoded from the file, empty while disabled or undecodable.
	source: Signal<{ video?: VideoStreamTrack; audio?: Audio.Source }>;
};

// Image, video, and audio files we know how to decode (see #decode).
const ACCEPT = "image/*,video/*,audio/*";

/** Decodes a local file into looping capture tracks. */
export class File {
	readonly in: Readonlys<FileInput>;

	/** The file to decode. Written by {@link prompt}, or set it directly. */
	file: Signal<globalThis.File | undefined>;

	readonly #out: FileOutput = {
		source: new Signal<{ video?: VideoStreamTrack; audio?: Audio.Source }>({}),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(props?: FileProps) {
		this.in = {
			enabled: getter(props?.enabled ?? false),
		};
		this.file = Signal.from(props?.file);

		this.#signals.run((effect) => {
			const values = effect.getAll([this.file, this.in.enabled]);
			if (!values) return;
			const [file] = values;

			this.#decode(file, effect).catch((err) => {
				console.error("Failed to decode file:", err);
			});
		});
	}

	/**
	 * Open a native file picker and use the chosen file as the source.
	 * Must be called from within a user gesture (e.g. a click handler), otherwise the browser blocks the dialog.
	 */
	prompt() {
		const input = document.createElement("input");
		input.type = "file";
		input.accept = ACCEPT;
		input.addEventListener("change", () => {
			const file = input.files?.[0];
			if (file) this.file.set(file);
		});
		input.click();
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

		effect.set(this.#out.source, { video: videoTrack as VideoStreamTrack }, {});
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
			this.#out.source,
			{
				video: videoTrack as VideoStreamTrack,
				audio: audioTrack ? { track: audioTrack as Audio.StreamTrack, kind: "auto" } : undefined,
			},
			{},
		);
	}

	/** Stop decoding and release the file. */
	close() {
		this.#signals.close();
	}
}
