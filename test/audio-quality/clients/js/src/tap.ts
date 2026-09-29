/**
 * Fan the tap worklet out from a player's output node, and collect what it reports.
 *
 * The tap reaches the destination through a zero gain, because Web Audio only renders what the
 * destination pulls: left dangling it would never be asked for a quantum. The zero gain keeps it out
 * of what the listener hears.
 *
 * @module
 */
import type { Counts } from "./quantum.ts";
import type { Gap } from "./schema.ts";
import TapWorklet from "./tap-worklet.ts?worklet";

/** What the worklet is constructed with. */
export type TapInit = {
	/** The silence floor, as an RMS level. */
	floor: number;
};

/** What the worklet posts every few dozen quanta. */
export type TapReport = {
	/** Cumulative since the first audible quantum. */
	counts: Counts;
	/** Gaps closed since the last report. */
	gaps: Gap[];
};

/** A running tap. */
export type Tap = {
	/** The latest cumulative counts, or undefined before the first report. */
	counts(): Counts | undefined;
	/** Gaps closed since the last call. */
	take(): Gap[];
	close(): void;
};

/** Attach a tap to `root`. */
export async function tap(root: AudioNode, init: TapInit): Promise<Tap> {
	const context = root.context as AudioContext;
	await context.audioWorklet.addModule(TapWorklet);

	const node = new AudioWorkletNode(context, "audio-quality-tap", {
		numberOfInputs: 1,
		numberOfOutputs: 1,
		outputChannelCount: [1],
		processorOptions: init,
	});
	const mute = new GainNode(context, { gain: 0 });
	root.connect(node);
	node.connect(mute);
	mute.connect(context.destination);

	let counts: Counts | undefined;
	const gaps: Gap[] = [];
	node.port.onmessage = (event: MessageEvent<TapReport>) => {
		counts = event.data.counts;
		gaps.push(...event.data.gaps);
	};

	return {
		counts: () => counts,
		take: () => gaps.splice(0, gaps.length),
		close() {
			node.port.onmessage = null;
			root.disconnect(node);
			node.disconnect();
			mute.disconnect();
		},
	};
}
