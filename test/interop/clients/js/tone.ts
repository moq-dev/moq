import { type Sample, TONE_CHECK } from "./src/contract";

/** How long rendered audio may play before it has to carry the tone: several analyser windows. */
const GRACE_MS = 3000;

type Observation = Pick<Sample, "at" | "audioContext" | "audioStalled" | "audioCodec" | "audioChannels" | "tones">;

/** Judge the expected tones after giving the running player a bounded grace to fill its analyser. */
export class ToneCheck {
	#expected: { codec: string; channels: number };
	#previous?: { at: number; playing: boolean };
	#played = 0;

	constructor(expected: { codec: string; channels: number }) {
		this.#expected = expected;
	}

	/** Wait through buffering, accept the tone, or fail after enough audio has played to judge it. */
	read(state: Observation): "waiting" | "heard" | "failed" {
		const playing = state.audioContext === "running" && !state.audioStalled;
		// Samples use the page's clock, so polling the same observation cannot spend the grace.
		// Both ends must be playing: a refill or suspension contributes no playback time.
		if (playing && this.#previous?.playing) this.#played += state.at - this.#previous.at;
		this.#previous = { at: state.at, playing };
		if (!playing) return "waiting";

		if (
			state.audioCodec?.split(".")[0] === this.#expected.codec &&
			state.audioChannels === this.#expected.channels &&
			state.tones?.length === this.#expected.channels &&
			state.tones.every(({ rms, share }) => rms >= TONE_CHECK.minRms && share >= TONE_CHECK.minShare)
		) {
			return "heard";
		}
		return this.#played >= GRACE_MS ? "failed" : "waiting";
	}
}
