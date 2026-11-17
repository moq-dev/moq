import { Duration } from "@moq/clock";
import type { AudioFrame } from "./capture";
import { Vad } from "./vad";

// How long a talker stays silent before the gate shuts, as libopus waits before DTX
// (NB_SPEECH_FRAMES_BEFORE_DTX, 10 frames of 20 ms). It carries a word's tail and some background
// for the listener's comfort noise to match.
const HANGOVER = Duration.millis(200);

/** @internal A frame on its way to the encoder, and where it ends. */
export interface Gated {
	frame: AudioFrame;
	end: Duration;
}

/** @internal Constructor options for {@link Gate}. */
export interface GateProps {
	/** The rate the frames arrive at, in Hz. */
	sampleRate: number;

	/** The codec's frame duration. */
	frame: Duration;
}

/**
 * @internal Drops a silent talker's codec frames before they are encoded, as libopus's DTX
 * would, but sending nothing at all instead of its keep-alives.
 *
 * SILK's voice activity detector scores the audio. Once it has been silent past a 200 ms hangover,
 * frames are held back instead of passed on. Speech reopens the gate, with the held frames covering
 * one analysis block sent first as pre-roll, so a soft onset the detector scored late is not
 * clipped. A gap in the frames' timeline, such as a mute, resets it shut.
 */
export class Gate {
	readonly #vad: Vad;
	// What reopening sends ahead of the speech: one analysis block of held frames.
	readonly #preroll: Duration;

	// Open counts how long the talker has been silent; shut keeps the newest dropped frames as pre-roll.
	#state: { open: true; quiet: Duration } | { open: false; held: Gated[] } = { open: false, held: [] };
	// The newest score, which a frame shorter than a block inherits until the next one completes.
	#speech = false;
	// Where the previous frame ended, to tell a gap in the timeline.
	#end: Duration | undefined;

	constructor(props: GateProps) {
		// SILK analyzes 10 ms frames for a 10 ms packet and 20 ms frames otherwise.
		const block = props.frame.lt(Duration.millis(20)) ? 10 : 20;
		this.#vad = new Vad({ sampleRate: props.sampleRate, block });
		this.#preroll = Duration.millis(block);
	}

	/** Whether frames are passing. A transition from open to shut is where the talker fell silent. */
	get open(): boolean {
		return this.#state.open;
	}

	/** Score one codec frame ending at `end`, returning the frames to encode now, in order. */
	push(frame: AudioFrame, end: Duration): Gated[] {
		if (this.#end !== undefined && !frame.timestamp.eq(this.#end)) this.#reset();
		this.#end = end;

		this.#speech = this.#vad.push(frame) ?? this.#speech;
		const gated = { frame, end };

		if (this.#speech) {
			const out = this.#state.open ? [gated] : [...this.#state.held, gated];
			this.#state = { open: true, quiet: Duration.ZERO };
			return out;
		}

		if (this.#state.open) {
			const quiet = this.#state.quiet.add(end.sub(frame.timestamp));
			if (quiet.le(HANGOVER)) {
				this.#state.quiet = quiet;
				return [gated];
			}
			this.#state = { open: false, held: [gated] };
			return [];
		}

		const held = this.#state.held;
		held.push(gated);
		// Keep only the newest frames that cover the pre-roll.
		while (held.length > 1 && end.sub(held[1].frame.timestamp).ge(this.#preroll)) held.shift();
		return [];
	}

	// A new timeline starts shut, with nothing held from the old one.
	#reset(): void {
		this.#state = { open: false, held: [] };
		this.#speech = false;
		this.#vad.reset();
	}
}
