/**
 * Samples a `<moq-watch>` every 250 ms, from its public signals and a tap on its output.
 *
 * Nothing here patches the player or reaches past its `out` surface, so a number it reports is one
 * any consumer could read. What the player does not publish (how full each render quantum was) is
 * read off its output by the tap in `tap.ts`, every quantum rather than a sample of them.
 *
 * Adapted from the black-box probe in `debug-findings/analysis/blackbox.js` on the reporter's fork
 * (`fperex/moq`, branch `debug/rt-audio`). See ../../../README.md.
 *
 * @module
 */
import type MoqWatch from "@moq/watch/element";
import { type Environment, SAMPLE_INTERVAL_MS, type Sample, SILENCE_RMS, type Stall } from "./schema.ts";
import { type Tap, tap } from "./tap.ts";

/** Chromium's render capacity surface, which is not in lib.dom. */
type RenderCapacity = {
	start(options: { updateInterval: number }): void;
	addEventListener(type: "update", listener: (event: { averageLoad: number }) => void): void;
};

/** What the probe has collected, until someone drains it. */
export type Probe = {
	/** Samples taken since the last drain. */
	drain(): Sample[];
	/** What the page is, once the catalog says the session is up. */
	environment(): Environment | undefined;
	/** Console warnings and errors so far. */
	notes(): string[];
	/** Whether the ring has a playhead and is not stalled. */
	playing(): boolean;
};

/** Start sampling `watch`, from before it has produced anything, so startup is on the record too. */
export function probe(watch: MoqWatch): Probe {
	const samples: Sample[] = [];
	const notes: string[] = [];
	const note = (line: string) => {
		if (notes.length < 200) notes.push(line.slice(0, 200));
	};

	const { audio, sync, broadcast } = watch.player;

	// Stall changes are taken as they happen: a re-stall shorter than the sample grid is exactly the
	// one a 250 ms read would miss, and it is what tells a stall from an underrun.
	let stalls: Stall[] = [];
	audio.out.stalled.subscribe((stalled) => {
		const context = audio.out.context.peek();
		if (context) stalls.push({ at: context.currentTime * 1000, stalled });
	});

	// The tap follows the output node, which the player rebuilds with its graph. A tap left on the old
	// node would report a perfect run as one long silence.
	let tapped: AudioNode | undefined;
	let current: Tap | undefined;
	let renderLoad: number | undefined;
	const attach = () => {
		const root = audio.out.root.peek();
		if (!root || root === tapped) return;
		tapped = root;
		current?.close();
		current = undefined;

		tap(root, { floor: SILENCE_RMS }).then(
			(t) => {
				if (tapped === root) current = t;
				else t.close();
			},
			(err: unknown) => note(`tap: ${err instanceof Error ? err.message : String(err)}`),
		);

		const capacity = (root.context as unknown as { renderCapacity?: RenderCapacity }).renderCapacity;
		if (!capacity) {
			note("renderCapacity: unavailable, render_load will be null");
			return;
		}
		capacity.addEventListener("update", (event) => {
			renderLoad = event.averageLoad;
		});
		// A whole second: shorter intervals are refused by some builds, and load is smooth anyway.
		capacity.start({ updateInterval: 1 });
	};

	const sample = (): Sample => {
		attach();
		const context = audio.out.context.peek();
		const counts = current?.counts();
		const gaps = current?.take() ?? [];
		const taken = stalls;
		stalls = [];
		return {
			// Read back to back, so the pair calibrates the render clock against the viewer's.
			at: performance.now(),
			render: context ? context.currentTime * 1000 : undefined,
			timestamp: audio.out.timestamp.peek(),
			stalled: audio.out.stalled.peek(),
			delay: sync.out.delay.peek(),
			outputLatency: context ? context.outputLatency * 1000 : undefined,
			baseLatency: context ? context.baseLatency * 1000 : undefined,
			renderLoad,
			quanta: counts?.quanta,
			quiet: counts?.quiet,
			gaps: gaps.length > 0 ? gaps : undefined,
			stalls: taken.length > 0 ? taken : undefined,
		};
	};

	// Console output is evidence: a decoder reset shows up here before it shows up in a count.
	for (const level of ["warn", "error"] as const) {
		const original = console[level];
		console[level] = (...args: unknown[]) => {
			note(`${level}: ${args.map(String).join(" ")}`);
			original(...args);
		};
	}

	setInterval(() => samples.push(sample()), SAMPLE_INTERVAL_MS);

	return {
		drain: () => samples.splice(0, samples.length),
		environment() {
			const catalog = broadcast.out.catalog.peek();
			if (!catalog) return undefined;
			// One rendition is published per broadcast here, so the first is the one playing.
			const config = Object.values(catalog.audio?.renditions ?? {})[0];
			return {
				crossOriginIsolated: globalThis.crossOriginIsolated === true,
				transport: watch.connection.transport.peek(),
				codec: config?.codec,
				rate: config?.sampleRate,
				jitter: config?.jitter,
				contextRate: audio.out.context.peek()?.sampleRate,
			};
		},
		notes: () => notes.slice(),
		playing: () => audio.out.timestamp.peek() !== undefined && !audio.out.stalled.peek(),
	};
}
