import * as Catalog from "@moq/hang/catalog";
import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";

export type ThumbnailFormat = "image/jpeg" | "image/png" | "image/webp";

export type ThumbnailProps = {
	// Whether thumbnail capture is enabled.
	enabled?: boolean | Signal<boolean>;

	// Minimum interval between thumbnails in milliseconds. Default: 5000.
	interval?: number | Signal<number>;

	// Image MIME type. Default: image/jpeg.
	format?: ThumbnailFormat | Signal<ThumbnailFormat>;

	// Quality (0-1) for jpeg/webp. Default: 0.7.
	quality?: number | Signal<number>;

	// Target thumbnail width in pixels. Height is derived from the source aspect.
	// Default: 320.
	width?: number | Signal<number>;

	// The source video frame to capture from.
	frame: Signal<VideoFrame | undefined>;
};

// Thumbnail captures still images from a live VideoFrame source and publishes
// them as a separate moq track. Images are JPEG/PNG/WebP encoded via
// OffscreenCanvas.convertToBlob(). One image is one group, one frame.
//
// Capture is rate-limited to at most one image every `interval` ms and only
// happens while the track is being subscribed (serve()), so an idle publisher
// with no paused viewers does no work and uses no bandwidth.
export class Thumbnail {
	static readonly TRACK = "thumbnail/lg";
	static readonly PRIORITY = Catalog.PRIORITY.thumbnail;

	enabled: Signal<boolean>;
	interval: Signal<number>;
	format: Signal<ThumbnailFormat>;
	quality: Signal<number>;
	width: Signal<number>;
	frame: Signal<VideoFrame | undefined>;

	catalog = new Signal<Catalog.Thumbnail | undefined>(undefined);

	#signals = new Effect();

	constructor(props: ThumbnailProps) {
		this.enabled = Signal.from(props.enabled ?? false);
		this.interval = Signal.from(props.interval ?? 5000);
		this.format = Signal.from(props.format ?? ("image/jpeg" as ThumbnailFormat));
		this.quality = Signal.from(props.quality ?? 0.7);
		this.width = Signal.from(props.width ?? 320);
		this.frame = props.frame;

		this.#signals.run(this.#runCatalog.bind(this));
	}

	#runCatalog(effect: Effect): void {
		if (!effect.get(this.enabled)) return;

		const frame = effect.get(this.frame);
		if (!frame) return;

		const width = effect.get(this.width);
		const sourceWidth = frame.codedWidth;
		const sourceHeight = frame.codedHeight;
		if (!sourceWidth || !sourceHeight) return;

		const height = Math.max(1, Math.round((sourceHeight * width) / sourceWidth));

		const config: Catalog.ThumbnailConfig = {
			codec: effect.get(this.format),
			container: { kind: "legacy" },
			codedWidth: Catalog.u53(width),
			codedHeight: Catalog.u53(height),
			interval: Catalog.u53(effect.get(this.interval)),
			quality: effect.get(this.quality),
		};

		effect.set(this.catalog, { renditions: { [Thumbnail.TRACK]: config } });
	}

	// Called by the broadcast when a viewer subscribes to the thumbnail track.
	// Captures one image per interval until the track is closed.
	serve(track: Moq.Track, effect: Effect): void {
		effect.run((effect) => {
			if (!effect.get(this.enabled)) return;
			const interval = effect.get(this.interval) ?? 5000;

			// Encode immediately so a freshly-subscribed viewer doesn't wait `interval`
			// for the first image, then continue at the configured cadence.
			this.#captureOnce(track, effect);
			effect.interval(() => this.#captureOnce(track, effect), interval);
		});
	}

	#captureOnce(track: Moq.Track, effect: Effect): void {
		const frame = this.frame.peek();
		if (!frame) return;

		const sourceWidth = frame.codedWidth;
		const sourceHeight = frame.codedHeight;
		if (!sourceWidth || !sourceHeight) return;

		const targetWidth = this.width.peek();
		const targetHeight = Math.max(1, Math.round((sourceHeight * targetWidth) / sourceWidth));

		const format = this.format.peek();
		const quality = this.quality.peek();
		// VideoFrame.timestamp is microseconds since some epoch chosen by the source.
		const timestamp = frame.timestamp;

		// Snapshot via OffscreenCanvas + convertToBlob. createImageBitmap clones the
		// frame's surface so we can release the original VideoFrame immediately.
		effect.spawn(async () => {
			let bitmap: ImageBitmap | undefined;
			let canvas: OffscreenCanvas | undefined;
			try {
				bitmap = await createImageBitmap(frame, {
					resizeWidth: targetWidth,
					resizeHeight: targetHeight,
					resizeQuality: "medium",
				});

				canvas = new OffscreenCanvas(targetWidth, targetHeight);
				const ctx = canvas.getContext("2d");
				if (!ctx) throw new Error("failed to get 2d context");
				ctx.drawImage(bitmap, 0, 0);

				const blob = await canvas.convertToBlob({ type: format, quality });
				const payload = new Uint8Array(await blob.arrayBuffer());

				const tsBytes = Moq.Varint.encode(Math.max(0, Math.floor(timestamp)));
				const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
				data.set(tsBytes, 0);
				data.set(payload, tsBytes.byteLength);

				// One image per group, group closed immediately. Subscribers can use
				// nextGroupOrdered() to skip directly to the latest thumbnail.
				track.writeFrame(data);
			} catch (err) {
				console.warn("thumbnail capture failed", err);
			} finally {
				bitmap?.close();
			}
		});
	}

	close() {
		this.#signals.close();
	}
}
