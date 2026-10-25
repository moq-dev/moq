import { Milli } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import { Error as NetError } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys } from "@moq/signals";
import type { VTTCue, VTTRegion } from "media-captions";
import { subscribeMedia } from "../media";
import type { Sync } from "../sync";
import type { Source } from "./source";

// Drop cues whose end is more than this far behind the playhead, to bound memory on a long stream.
const PRUNE_BEHIND = Milli(30_000);

// The longest a cue shows. A `utf8` roll-up cue lingers this long when no later cue supersedes it,
// and a longer declared end is clamped to it, so every cue eventually falls behind the prune horizon.
const MAX_CUE = Milli(30_000);

/**
 * The most cues, and separately clear marks, one track retains, evicting the earliest start first.
 *
 * The prune horizon keeps an honest stream far below this. It bounds a publisher whose timestamps
 * run ahead of the playhead, because the renderer rescans every retained cue on each animation frame
 * and rebuilds them all on each group. Exported for tests.
 *
 * @internal
 */
export const MAX_RETAINED = 1024;

// How far a VTT payload's own timing may sit from its frame timestamp before we call it a bug.
// Generous: this is catching a wrong clock, not measuring drift.
const SKEW_TOLERANCE = Milli(1_000);

/**
 * The cue state for one selected track, accumulated as groups arrive.
 *
 * `clears` holds the timestamps of `utf8` clear frames. They outlive the cue they terminate because
 * groups are read concurrently and can complete out of order: a clear may land before the cue it is
 * meant to end, and has to still apply when that cue shows up.
 *
 * @internal
 */
export type CueStore = {
	cues: VTTCue[];
	clears: number[];
};

/**
 * Neutralize a cue payload before the renderer splices it into `innerHTML`.
 *
 * [media-captions](https://github.com/vidstack/captions) decodes a fixed set of HTML entities in
 * cue text *after* tokenizing it, then concatenates the result into an HTML string and assigns that
 * to `innerHTML`. So `&amp;lt;img src=x onerror=…&amp;gt;` never presents a literal `<` for the
 * tokenizer to intercept, gets decoded once it is too late, and becomes a live element whose
 * `onerror` runs script in the page's origin. Its tag allowlist does not help, and
 * `pointer-events: none` does not stop a load handler. Annotation values (`<v Name>`) are also
 * interpolated into attributes without quote escaping.
 *
 * Escaping one level deeper than usual means that single decode pass lands on the ordinary escape,
 * so the text renders exactly as the publisher wrote it and no markup survives. `&amp;` becomes
 * `&amp;amp;amp;`, decodes to `&amp;amp;`, and displays as `&amp;`. Since no raw `<` is left, no tags
 * are built and the attribute hole closes with it.
 *
 * VTT styling tags are lost, which is the same trade the publish side already makes when it escapes
 * outgoing cues. Rendering an allowlist of WebVTT elements as real DOM nodes is the upgrade path.
 *
 * @internal
 */
export function sanitizeCue(text: string): string {
	return text
		.replaceAll("&", "&amp;amp;")
		.replaceAll("<", "&amp;lt;")
		.replaceAll(">", "&amp;gt;")
		.replaceAll('"', "&amp;quot;");
}

// Merge `incoming` into `stored`, both sorted by `key`, keeping the latest MAX_RETAINED. Sorting the
// payload once and merging in one pass keeps a payload of n entries O(n log n); a splice per entry
// would be quadratic.
function merge<T>(stored: T[], incoming: T[], key: (value: T) => number): T[] {
	const merged: T[] = [];
	let i = 0;
	// Not `toSorted`: Chrome and Edge 97-109 lack it, and `sort` is stable, so ties keep arrival order.
	for (const value of [...incoming].sort((a, b) => key(a) - key(b))) {
		while (i < stored.length && key(stored[i]) <= key(value)) merged.push(stored[i++]);
		merged.push(value);
	}
	while (i < stored.length) merged.push(stored[i++]);
	return merged.length > MAX_RETAINED ? merged.slice(-MAX_RETAINED) : merged;
}

/**
 * Add one payload's cues to the store, keeping it sorted by start time.
 *
 * Groups arrive in delivery order rather than sequence order, so a cue can legitimately land
 * before one that is already stored. Each end is clamped to `MAX_CUE` past its start, so a
 * declared end cannot hold a cue past the prune horizon. Exported for tests.
 *
 * @internal
 */
export function insertCues(store: CueStore, cues: VTTCue[]): void {
	for (const cue of cues) cue.endTime = Math.min(cue.endTime, cue.startTime + MAX_CUE / 1000);
	store.cues = merge(store.cues, cues, (cue) => cue.startTime);
}

/**
 * The regions the retained cues draw into, so a region lives exactly as long as a cue using it.
 *
 * The parser only links a cue to a region declared in the same payload, so a region no retained cue
 * references can never be used again. Exported for tests.
 *
 * @internal
 */
export function cueRegions(cues: VTTCue[]): VTTRegion[] {
	const regions = new Map<string, VTTRegion>();
	for (const cue of cues) {
		if (cue.region) regions.set(cue.region.id, cue.region);
	}
	return [...regions.values()];
}

/**
 * Drop every cue that ended before `cutoff`, returning whether anything was removed.
 *
 * Scans the whole array rather than shifting off the front: cues are ordered by start time, not
 * end time, so one long-running early cue would otherwise stop the sweep and let every expired cue
 * behind it accumulate for the life of the stream. Exported for tests.
 *
 * @internal
 */
export function pruneCues(store: CueStore, cutoff: number): boolean {
	const { cues, clears } = store;

	// Clear marks age out on the same horizon; nothing arriving now can precede them.
	let keptClears = 0;
	for (const t of clears) {
		if (t >= cutoff) clears[keptClears++] = t;
	}
	clears.length = keptClears;

	let kept = 0;
	for (const cue of cues) {
		if (cue.endTime >= cutoff) cues[kept++] = cue;
	}
	if (kept === cues.length) return false;
	cues.length = kept;
	return true;
}

/**
 * Clamp a `utf8` roll-up cue against its neighbours, so exactly one caption shows at a time.
 *
 * A roll-up cue has no real end: it runs until the next one starts. Both neighbours are checked
 * because the following cue may already have arrived out of order. Exported for tests.
 *
 * @internal
 */
export function rollUp(store: CueStore, cue: VTTCue): void {
	const { cues, clears } = store;
	const i = cues.indexOf(cue);
	// Evicted on arrival: it starts before everything a full store retains.
	if (i < 0) return;
	const prev = i > 0 ? cues[i - 1] : undefined;
	if (prev && prev.endTime > cue.startTime) prev.endTime = cue.startTime;

	const next = cues[i + 1];
	if (next && cue.endTime > next.startTime) cue.endTime = next.startTime;

	// A clear that already arrived ends this cue, even though the cue is the later delivery.
	const clear = clears.find((t) => t > cue.startTime);
	if (clear !== undefined && cue.endTime > clear) cue.endTime = clear;
}

/**
 * End whichever roll-up cue is showing at `time`, adding nothing.
 *
 * An empty `utf8` payload clears the caption rather than displaying an empty one, so the clear is
 * a scheduled event in its own right. Exported for tests.
 *
 * @internal
 */
export function clearCue(store: CueStore, time: number): void {
	// Remember it first: the cue this clear terminates may not have arrived yet.
	if (!store.clears.includes(time)) store.clears = merge(store.clears, [time], (t) => t);

	const { cues } = store;
	for (let i = cues.length - 1; i >= 0; i--) {
		const cue = cues[i];
		if (cue.startTime > time) continue;
		if (cue.endTime > time) cue.endTime = time;
		return;
	}
}

/** Signals the renderer reads. */
export type RendererInput = {
	// The overlay element captions are drawn into. Owned by the caller (e.g. the <moq-watch> element),
	// which positions it over the video canvas.
	container: Getter<HTMLElement | undefined>;

	// Whether to subscribe and render. Defaults to true; the parent may gate this (e.g. on connection).
	enabled: Getter<boolean>;
};

/** Constructor properties for {@link Renderer}. */
export type RendererProps = Inputs<RendererInput> & {
	/** Caption rendition selector. */
	source: Source;
	/** Shared playback clock. */
	sync: Sync;
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
	/** The source picking the caption track. */
	readonly source: Source;
	/** The clock that schedules each cue. */
	readonly sync: Sync;
	/** The wired inputs, read-only. */
	readonly in: Readonlys<RendererInput>;

	#signals = new Effect();

	// Whether the clock-skew warning has already fired for the current track, so a misconfigured
	// publisher costs one line instead of one per cue.
	#skewWarned = false;

	constructor(props: RendererProps) {
		this.source = props.source;
		this.sync = props.sync;
		this.in = {
			container: getter(props?.container),
			enabled: getter(props?.enabled ?? true),
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
		const active = broadcast.relativeBroadcast(effect, config.broadcast, config.epoch);
		if (!active) return;

		// A new track gets its own chance to complain about its clock.
		this.#skewWarned = false;

		// Pick the frame container. Text uses the timestamped `legacy` container or LOC; anything else
		// (e.g. `cmaf`) would misread the cue payload, so skip it rather than render garbage.
		let format: Container.Format;
		if (config.container.kind === "legacy") {
			format = new Container.Legacy.Format("data");
		} else if (config.container.kind === "loc") {
			format = new Container.Loc.Format();
		} else {
			console.warn(`captions: unsupported container "${config.container.kind}" for track ${track}`);
			return;
		}

		const abort = effect.abort;
		effect.spawn(async () => {
			const { CaptionsRenderer, OVERLAY_CSS } = await import("./captions");
			if (abort.aborted) return;
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

			const sub = subscribeMedia(effect, {
				broadcast: active,
				track: Catalog.track(track, config),
				priority: Catalog.PRIORITY.text,
				maxDelay: this.sync.out.maxDelay,
			});
			if (!sub) return;
			const store: CueStore = { cues: [], clears: [] };
			const commit = () => renderer.changeTrack({ cues: store.cues, regions: cueRegions(store.cues) });

			// Drive the caption clock off the shared media clock so cues line up with audio/video, and
			// prune cues that have scrolled well past the playhead. Reschedules itself each frame.
			const tick = () => {
				const now = this.sync.now();
				if (now !== undefined) {
					renderer.currentTime = now / 1000;

					if (pruneCues(store, (now - PRUNE_BEHIND) / 1000)) commit();
				}
				effect.animate(tick);
			};
			effect.animate(tick);

			// Accept groups and read each in its own task, like `Container.Consumer` does for media. A
			// group whose stream stalls then delays only its own cue instead of every later one.
			effect.spawn(async () => {
				for (;;) {
					const group = await sub.recvGroup().catch((err) => {
						if (!(err instanceof NetError.Stream)) throw err;
						console.debug("captions subscription ended", err);
						return undefined;
					});
					if (!group) break;

					effect.spawn(async () => {
						try {
							for (;;) {
								const frame = await group.readFrame();
								if (!frame) break;
								for (const sample of format.decode(frame.payload, frame.timestamp)) {
									await this.#ingest(config.format, sample, store);
								}
							}
						} catch (err) {
							if (!(err instanceof NetError.Stream)) throw err;
						} finally {
							group.close();
						}

						commit();
					});
				}
			});
		});
	}

	// Parse one cue payload into the cue/region store. The frame timestamp is the authoritative cue
	// start on the media clock; a VTT payload also carries its own absolute timing.
	async #ingest(format: string, sample: Container.Frame, store: CueStore): Promise<void> {
		const { VTTCue, parseText } = await import("./captions");
		const text = new TextDecoder().decode(sample.payload);
		const start = (sample.timestamp as number) / 1e6;

		if (format === "utf8") {
			// An empty payload clears the caption (per the hang draft) rather than showing nothing,
			// so it still has to be scheduled: it just ends the current cue instead of adding one.
			if (text.length === 0) {
				clearCue(store, start);
				return;
			}

			const cue = new VTTCue(start, start + MAX_CUE / 1000, sanitizeCue(text));
			insertCues(store, [cue]);
			rollUp(store, cue);
			return;
		}

		if (text.length === 0) return;

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

		// The payload is untrusted, and the renderer builds HTML out of this text.
		for (const cue of parsed.cues) cue.text = sanitizeCue(cue.text);
		insertCues(store, parsed.cues);
	}

	/** Stop and release everything this holds. */
	close(): void {
		this.#signals.close();
	}
}
