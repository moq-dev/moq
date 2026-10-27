/** Real watch elements and a deterministic Web Audio gain probe. @module */
import "@moq/watch/element";
import "@moq/watch/ui";
import { Audio, Signals } from "@moq/watch";
import type MoqWatch from "@moq/watch/element";

/** Render a constant through the shipped emitter, driven by an element's real controls. */
export async function gain(watch: MoqWatch): Promise<{ level: number; sample: number; enabled: boolean }> {
	const context = new OfflineAudioContext(1, 48000, 48000);
	const constant = new ConstantSourceNode(context, { offset: 1 });
	let node: GainNode | undefined;
	// Only PCM production is a stand-in: context, gain automation, and rendering are real Web Audio.
	const source = {
		out: {
			root: new Signals.Signal({
				context,
				connect(gain: GainNode) {
					node = gain;
					constant.connect(gain);
				},
			}),
		},
	} as unknown as Audio.Decoder;
	const emitter = new Audio.Emitter({ source, ...watch.emitter.in });
	try {
		for (let i = 0; i < 5; i++) await new Promise<void>((resolve) => queueMicrotask(resolve));
		constant.start();
		const buffer = await context.startRendering();
		if (!node) throw new Error("emitter did not create a gain node");
		return {
			level: node.gain.value,
			sample: buffer.getChannelData(0).at(-1) ?? NaN,
			enabled: emitter.out.enabled.peek(),
		};
	} finally {
		emitter.close();
	}
}
