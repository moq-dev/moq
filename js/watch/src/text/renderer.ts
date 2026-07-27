import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys } from "@moq/signals";
import { CaptionsRenderer, parseText, VTTCue, type VTTRegion } from "media-captions";
// media-captions positions and styles cues purely through these stylesheets (via `[part]`
// attributes and CSS variables it sets); without them the overlay renders nothing visible.
import captionsCss from "media-captions/styles/captions.css?inline";
import regionsCss from "media-captions/styles/regions.css?inline";
import type { Sync } from "../sync";
import type { Source } from "./source";

// The renderer positions cues relative to a `[part="captions"]` box using the stylesheets above.
const OVERLAY_CSS = `${captionsCss}\n${regionsCss}`;

// Drop cues whose end is more than this far behind the playhead, to bound memory on a long stream.
const PRUNE_BEHIND = Time.Milli(30_000);

// How long a `utf8` roll-up cue lingers when no later cue supersedes it.
const UTF8_LINGER = Time.Milli(30_000);

// How far a VTT payload's own timing may sit from its frame timestamp before we call it a bug.
// Generous: this is catching a wrong clock, not measuring drift.
const SKEW_TOLERANCE = Time.Milli(1_000);

/**
 * Insert a cue keeping `cues` sorted by start time.
 *
 * Groups arrive in delivery order rather than sequence order, so a cue can legitimately land
 * before one that is already stored. Exported for tests.
 *
 * @internal
 */
export function insertCue(cues: VTTCue[], cue: VTTCue): void {
	let i = cues.length;
	while (i > 0 && cues[i - 1].startTime > cue.startTime) i--;
	cues.splice(i, 0, cue);
}

/**
 * Clamp a `utf8` roll-up cue against its neighbours, so exactly one caption shows at a time.
 *
 * A roll-up cue has no real end: it runs until the next one starts. Both neighbours are checked
 * because the following cue may already have arrived out of order. Exported for tests.
 *
 * @internal
 */
export function rollUp(cues: VTTCue[], cue: VTTCue): void {
	const i = cues.indexOf(cue);
	const prev = i > 0 ? cues[i - 1] : undefined;
	if (prev && prev.endTime > cue.startTime) prev.endTime = cue.startTime;

	const next = cues[i + 1];
	if (next && cue.endTime > next.startTime) cue.endTime = next.startTime;
}

export type RendererInput = {
	// The overlay element captions are drawn into. Owned by the caller (e.g. the <moq-watch> element),
	// which positions it over the video canvas.
	container: Getter<HTMLElement | undefined>;

	// Whether to subscribe and render. The parent gates this (e.g. on connection).
	enabled: Getter<boolean>;
};

/**
 * Subscribes to the selected caption track, parses each cue, and renders it into an overlay element
 * via [media-captions](https://github.com/vidstack/captions).
 *
 * There is no WebCodecs decoder for text, so this parses cue payloads directly. Cues are scheduled
 * against the shared {@link Sync} clock (not wall time) so they line up with the audio/video frames.
 * A caption-only broadcast (no audio/video to anchor the clock) shows nothing until media arrives.
 */
export class Renderer {
	readonly source: Source;
	readonly sync: Sync;
	readonly in: Readonlys<RendererInput>;

	#signals = new Effect();

	// Whether the clock-skew warning has already fired for the current track, so a misconfigured
	// publisher costs one line instead of one per cue.
	#skewWarned = false;

	constructor(source: Source, sync: Sync, props?: Inputs<RendererInput>) {
		this.source = source;
		this.sync = sync;
		this.in = {
			container: getter(props?.container),
			enabled: getter(props?.enabled ?? false),
		};

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const values = effect.getAll([
			this.in.enabled,
			this.in.container,
			this.source.out.track,
			this.source.out.config,
			this.source.in.broadcast,
		]);
		if (!values) return;
		const [_enabled, container, track, config, broadcast] = values;

		// Honor a per-rendition `broadcast` override: subscribe on the resolved source broadcast.
		const active = broadcast.relativeBroadcast(effect, config.broadcast);
		if (!active) return;

		// A new track gets its own chance to complain about its clock.
		this.#skewWarned = false;

		// Pick the frame container. Text uses the timestamped `legacy` container or LOC; anything else
		// (e.g. `cmaf`) would misread the cue payload, so skip it rather than render garbage.
		let format: Container.Format;
		if (config.container.kind === "legacy") {
			format = new Container.Legacy.Format();
		} else if (config.container.kind === "loc") {
			format = new Container.Loc.Format();
		} else {
			console.warn(`captions: unsupported container "${config.container.kind}" for track ${track}`);
			return;
		}

		// The media-captions stylesheet, mounted as a sibling of the overlay (its `[part]` selectors are
		// global). It can't live inside the overlay: the renderer clears `overlay.textContent` on every
		// track change, which would wipe it.
		const style = document.createElement("style");
		style.textContent = OVERLAY_CSS;
		container.appendChild(style);
		effect.cleanup(() => style.remove());

		// A fresh overlay per selection, so cleanup is just removing it.
		const overlay = document.createElement("div");
		overlay.style.position = "absolute";
		overlay.style.inset = "0";
		overlay.style.pointerEvents = "none";
		container.appendChild(overlay);
		effect.cleanup(() => overlay.remove());

		const renderer = new CaptionsRenderer(overlay);
		// The renderer installs a ResizeObserver; destroy() disconnects it (removing the overlay alone
		// does not), so a track switch or reconnect doesn't leak an observer.
		effect.cleanup(() => renderer.destroy());

		const sub = active.track(track).subscribe({ priority: Catalog.PRIORITY.text });
		effect.cleanup(() => sub.close());
		const cues: VTTCue[] = [];
		const regions = new Map<string, VTTRegion>();
		const commit = () => renderer.changeTrack({ cues: [...cues], regions: [...regions.values()] });

		// Drive the caption clock off the shared media clock so cues line up with audio/video, and
		// prune cues that have scrolled well past the playhead. Reschedules itself each frame.
		const tick = () => {
			const now = this.sync.now();
			if (now !== undefined) {
				renderer.currentTime = now / 1000;

				const cutoff = (now - PRUNE_BEHIND) / 1000;
				let pruned = false;
				while (cues.length > 0 && cues[0].endTime < cutoff) {
					cues.shift();
					pruned = true;
				}
				if (pruned) commit();
			}
			effect.animate(tick);
		};
		effect.animate(tick);

		// Accept groups and read each in its own task, like `Container.Consumer` does for media. A
		// group whose stream stalls then delays only its own cue instead of every later one.
		effect.spawn(async () => {
			for (;;) {
				const group = await sub.recvGroup();
				if (!group) break;

				effect.spawn(async () => {
					try {
						for (;;) {
							const frame = await group.readFrame();
							if (!frame) break;
							for (const sample of format.decode(frame.payload)) {
								await this.#ingest(config.format, sample, cues, regions);
							}
						}
					} catch (err) {
						console.warn("captions: group read error", err);
					} finally {
						group.close();
					}

					commit();
				});
			}
		});
	}

	// Parse one cue payload into the cue/region store. The frame timestamp is the authoritative cue
	// start on the media clock; a VTT payload also carries its own absolute timing.
	async #ingest(
		format: string,
		sample: Container.Frame,
		cues: VTTCue[],
		regions: Map<string, VTTRegion>,
	): Promise<void> {
		const text = new TextDecoder().decode(sample.payload);
		if (text.length === 0) return;

		const start = (sample.timestamp as number) / 1e6;

		if (format === "utf8") {
			const cue = new VTTCue(start, start + UTF8_LINGER / 1000, text);
			insertCue(cues, cue);
			rollUp(cues, cue);
			return;
		}

		// vtt: a self-contained WEBVTT segment. `strict: false` (the default) tolerates a missing header.
		let parsed: Awaited<ReturnType<typeof parseText>>;
		try {
			parsed = await parseText(text, { type: "vtt" });
		} catch (err) {
			console.warn("captions: failed to parse VTT cue", err);
			return;
		}

		// The spec requires the payload's timing to be absolute on the same clock as the frame
		// timestamp. A publisher emitting segment-relative times (the usual WebVTT-in-fMP4 habit)
		// would otherwise place every cue silently wrong, so say so once rather than never.
		const first = parsed.cues.at(0);
		if (!this.#skewWarned && first && Math.abs(first.startTime - start) > SKEW_TOLERANCE / 1000) {
			this.#skewWarned = true;
			console.warn(
				`captions: cue timing is ${(first.startTime - start).toFixed(3)}s off its frame timestamp; ` +
					"the payload must carry absolute times on the media clock",
			);
		}

		for (const region of parsed.regions) regions.set(region.id, region);
		for (const cue of parsed.cues) insertCue(cues, cue);
	}

	close(): void {
		this.#signals.close();
	}
}
