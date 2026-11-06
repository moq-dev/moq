/** Real capture measurements isolating format overrides before encoding or playout. */
import * as Publish from "@moq/publish";
import { Effect } from "@moq/signals";

declare global {
	interface Window {
		captureMatrix: () => Promise<unknown[]>;
	}
}

window.captureMatrix = async () => {
	const results = [];
	for (const mode of ["none", "rate", "channels", "both"]) {
		const source = new AudioContext({ sampleRate: 48_000 });
		const oscillator = source.createOscillator();
		const destination = source.createMediaStreamDestination();
		oscillator.connect(destination);
		oscillator.start();
		await source.resume();
		const track = destination.stream.getAudioTracks()[0];
		const capture = new Publish.Audio.Capture({
			source: { track: track as Publish.Audio.StreamTrack, kind: "music" },
			sampleRate: mode === "rate" || mode === "both" ? 48_000 : undefined,
			channelCount: mode === "channels" || mode === "both" ? 1 : undefined,
		});
		const effect = new Effect();
		try {
			let fanout = capture.out.frames.peek();
			while (!fanout) {
				await capture.out.frames.changed();
				fanout = capture.out.frames.peek();
			}
			const reader = fanout.subscribe(effect).getReader();
			const channelsAudible: boolean[] = [];
			let gaps = 0;
			let expected: number | undefined;
			for (let i = 0; i < 512; i++) {
				const { value: frame } = await reader.read();
				if (!frame) throw new Error("capture ended");
				for (const [channel, samples] of frame.channels.entries()) {
					if (!samples.length || samples.some((sample) => !Number.isFinite(sample))) {
						throw new Error(`capture channel ${channel} has invalid PCM`);
					}
					const rms = Math.sqrt(samples.reduce((sum, value) => sum + value * value, 0) / samples.length);
					channelsAudible[channel] = (channelsAudible[channel] ?? false) || rms > 0.01;
				}
				if (expected !== undefined && Math.abs(frame.timestamp - expected) > 2) gaps++;
				expected = frame.timestamp + (frame.channels[0].length / 48_000) * 1_000_000;
			}
			await reader.cancel();
			results.push({ mode, channelsAudible, gaps, format: capture.out.format.peek() });
		} finally {
			effect.close();
			capture.close();
			track.stop();
			oscillator.stop();
			await source.close();
		}
	}
	return results;
};
