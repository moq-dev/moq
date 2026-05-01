import * as Catalog from "@moq/hang/catalog";
import * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";

export interface ThumbnailProps {
	enabled?: boolean | Signal<boolean>;
}

// Subscribes to the publisher's thumbnail track and decodes the latest image.
//
// The thumbnail is exposed as an ImageBitmap (drawable to a canvas) and is
// distinct from a decoded video frame: thumbnails are full encoded images
// (JPEG/PNG/WebP) that arrive sparsely, not WebCodecs VideoFrames.
//
// While disabled, no subscription is created, so an opted-out player has zero
// idle bandwidth from this module.
export class Thumbnail {
	enabled: Signal<boolean>;

	// The most recently decoded thumbnail. Renderer can draw this when the player
	// is paused with no decoded video frame yet.
	bitmap = new Signal<ImageBitmap | undefined>(undefined);

	#broadcast: Getter<Moq.Broadcast | undefined>;
	#catalog: Getter<Catalog.Root | undefined>;
	#signals = new Effect();

	constructor(
		broadcast: Getter<Moq.Broadcast | undefined>,
		catalog: Getter<Catalog.Root | undefined>,
		props?: ThumbnailProps,
	) {
		this.#broadcast = broadcast;
		this.#catalog = catalog;
		this.enabled = Signal.from(props?.enabled ?? false);

		this.#signals.run(this.#runSubscription.bind(this));
	}

	#runSubscription(effect: Effect): void {
		if (!effect.get(this.enabled)) return;

		const broadcast = effect.get(this.#broadcast);
		if (!broadcast) return;

		const catalog = effect.get(this.#catalog)?.thumbnail;
		if (!catalog) return;

		// Pick the first available rendition for v1. In the future we could
		// pick the rendition whose codedWidth is closest to the canvas size.
		const entry = Object.entries(catalog.renditions)[0];
		if (!entry) return;
		const [trackName, config] = entry as [string, Catalog.ThumbnailConfig];

		const track = broadcast.subscribe(trackName, Catalog.PRIORITY.thumbnail);
		effect.cleanup(() => track.close());
		effect.cleanup(() =>
			this.bitmap.update((prev) => {
				prev?.close();
				return undefined;
			}),
		);

		effect.spawn(async () => {
			try {
				for (;;) {
					// nextGroupOrdered() always advances to the latest sequence,
					// silently dropping groups older than the last one returned.
					const group = await Promise.race([effect.cancel, track.nextGroupOrdered()]);
					if (!group) break;

					try {
						const raw = await group.readFrame();
						if (!raw) continue;

						// Strip the legacy-container VarInt timestamp prefix.
						const [, payload] = Moq.Varint.decode(raw);

						// Copy into a fresh ArrayBuffer so TS doesn't complain about
						// SharedArrayBuffer-backed Uint8Arrays in the BlobPart union.
						const buf = new Uint8Array(payload.byteLength);
						buf.set(payload);
						const blob = new Blob([buf.buffer], { type: config.codec });
						const next = await createImageBitmap(blob);

						this.bitmap.update((prev) => {
							prev?.close();
							return next;
						});
					} finally {
						group.close();
					}
				}
			} catch (err) {
				console.warn("thumbnail decode failed", err);
			}
		});
	}

	close() {
		this.#signals.close();
		this.bitmap.update((prev) => {
			prev?.close();
			return undefined;
		});
	}
}
