import { Duration } from "@moq/clock";
import { Analysis } from "./analysis";
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
 * Each frame is scored as libopus scores it at complexity 7 and up, what both WebCodecs on desktop
 * and the Rust publisher run: its tonality analysis, or SILK's detector at 8 and 12 kHz, where
 * libopus runs no analysis. Rates Opus does not take go through the analysis at 48 kHz.
 * Once the talker has been silent past a 200 ms hangover, frames are held back instead of passed
 * on. Speech reopens the gate, with the held frames covering one analysis step sent first as
 * pre-roll, so a soft onset scored late is not clipped. A mute, or any other gap in the frames'
 * timeline, resets it shut.
 */
export class Gate {
	readonly #detector: { kind: "analysis"; analysis: Analysis } | { kind: "silk"; vad: Vad };
	// What reopening sends ahead of the speech: one analysis step of held frames.
	readonly #preroll: Duration;
	// The samples libopus decides each piece of a longer frame on, if it splits them.
	readonly #piece: number | undefined;
	readonly #sampleRate: number;

	// Open counts how long the talker has been silent; shut keeps the newest dropped frames as pre-roll.
	#state: { open: true; quiet: Duration } | { open: false; held: Gated[] } = { open: false, held: [] };
	// SILK's newest score, which a frame shorter than its block inherits until the next one completes.
	#speech = false;
	// Where the previous frame ended, to tell a gap in the timeline.
	#end: Duration | undefined;

	constructor(props: GateProps) {
		const rate = props.sampleRate;
		this.#sampleRate = rate;
		// libopus runs no analysis at 8 and 12 kHz, so SILK's detector decides alone there.
		if (rate === 8000 || rate === 12000) {
			// SILK analyzes 10 ms frames for a 10 ms packet and 20 ms frames otherwise.
			const block = props.frame.lt(Duration.millis(20)) ? 10 : 20;
			this.#detector = { kind: "silk", vad: new Vad({ sampleRate: rate, block }) };
			this.#preroll = Duration.millis(block);
		} else {
			this.#detector = { kind: "analysis", analysis: new Analysis({ sampleRate: rate }) };
			this.#preroll = Duration.millis(20);
			// Hybrid and CELT packets over 20 ms are coded, and so decided, 20 ms at a time. That is
			// every rate above 16 kHz at voice bitrates; SILK alone (16 kHz, or under about 16 kbps)
			// decides the whole frame.
			this.#piece = rate > 16_000 ? rate / 50 : undefined;
		}
	}

	/** Whether frames are passing. A transition from open to shut is where the talker fell silent. */
	get open(): boolean {
		return this.#state.open;
	}

	/** Score one codec frame ending at `end`, returning the frames to encode now, in order. */
	push(frame: AudioFrame, end: Duration): Gated[] {
		if (this.#end !== undefined && !frame.timestamp.eq(this.#end)) this.reset();
		this.#end = end;

		// The silence after the newest activity, undefined once past the hangover. A frame passes if
		// any piece of it ends within the hangover, as libopus sends a packet unless every frame in
		// it is DTX.
		let quiet = this.#state.open ? this.#state.quiet : undefined;
		let passes = false;
		for (const piece of this.#split(frame)) {
			if (this.#active(piece)) quiet = Duration.ZERO;
			else if (quiet !== undefined) {
				const samples = piece.channels[0].length;
				quiet = quiet.add(Duration.micros((samples * 1_000_000) / this.#sampleRate));
				if (quiet.gt(HANGOVER)) quiet = undefined;
			}
			passes ||= quiet !== undefined;
		}

		const gated = { frame, end };
		if (passes) {
			const out = this.#state.open ? [gated] : [...this.#state.held, gated];
			// A frame whose last piece crossed the hangover is sent, so it is not held as pre-roll.
			this.#state = quiet !== undefined ? { open: true, quiet } : { open: false, held: [] };
			return out;
		}

		const held = this.#state.open ? [] : this.#state.held;
		held.push(gated);
		// Keep only the newest frames that cover the pre-roll.
		while (held.length > 1 && end.sub(held[1].frame.timestamp).ge(this.#preroll)) held.shift();
		this.#state = { open: false, held };
		return [];
	}

	/** Start shut with nothing held, as after a mute, keeping what the detector has learned. */
	reset(): void {
		this.#state = { open: false, held: [] };
		this.#speech = false;
		// The analysis keeps going across the break, as libopus would with the frames on either side.
		if (this.#detector.kind === "silk") this.#detector.vad.reset();
	}

	#active(piece: AudioFrame): boolean {
		if (this.#detector.kind === "analysis") return this.#detector.analysis.push(piece);
		this.#speech = this.#detector.vad.push(piece) ?? this.#speech;
		return this.#speech;
	}

	#split(frame: AudioFrame): AudioFrame[] {
		const piece = this.#piece;
		const samples = frame.channels[0].length;
		if (piece === undefined || samples <= piece || samples % piece !== 0) return [frame];
		return Array.from({ length: samples / piece }, (_, i) => ({
			timestamp: frame.timestamp.add(Duration.micros((i * piece * 1_000_000) / this.#sampleRate)),
			channels: frame.channels.map((channel) => channel.subarray(i * piece, (i + 1) * piece)),
		}));
	}
}
