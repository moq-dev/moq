// Builds the time stretch's conformance corpus described in /concept/audio-jitter.
//
// Run `bun doc/concept/audio-jitter/stretch/corpus.ts` to rewrite the JSON; `corpus.test.ts`
// fails when the checked-in files no longer match, so the corpus cannot be edited by hand to
// make a failing implementation pass.

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { type Decision, decide, HOLD, type Inputs, play } from "./reference";

/** One decision the rule must reach. */
export interface Rule extends Inputs {
	name: string;
	about: string;
	rate: number;
	decision: Decision;
}

/** Input PCM played hop by hop under a series of decisions, and the output it must produce. */
export interface Case {
	name: string;
	about: string;
	rate: number;
	channels: number;
	/** Interleaved input, every frame of it available from the start, as six decimals a float holds. */
	input: number[];
	/** The decision at each hop boundary, the last one repeated for every hop after it. */
	decisions: Decision[];
	/** The shift each hop's splice took, zero for none. */
	shifts: number[];
	/** Interleaved output, to seven decimals, well inside the tolerance it is compared to. */
	output: number[];
}

// A deterministic generator, so the corpus regenerates byte for byte.
function noise(seed: number): () => number {
	let state = seed >>> 0 || 1;
	return () => {
		state ^= state << 13;
		state >>>= 0;
		state ^= state >>> 17;
		state ^= state << 5;
		state >>>= 0;
		return state / 0x1_0000_0000 - 0.5;
	};
}

/**
 * A voiced talker: four harmonics of a pitch gliding from `low` to `high` Hz, under a syllable
 * envelope and a breath of noise, so no two shifts match equally well.
 */
function voiced(rate: number, frames: number, low: number, high: number, seed = 1): Float32Array {
	const out = new Float32Array(frames);
	const breath = noise(seed);
	let phase = 0;
	for (let i = 0; i < frames; i++) {
		const t = i / frames;
		phase += (2 * Math.PI * (low + (high - low) * t)) / rate;
		const envelope = 0.6 + 0.4 * Math.sin(2 * Math.PI * 3 * (i / rate));
		let sum = 0;
		for (let n = 1; n <= 4; n++) sum += Math.sin(n * phase) / n;
		out[i] = 0.25 * envelope * sum + 0.01 * breath();
	}
	return out;
}

/** Two notes a third apart, whose combined waveform repeats far more slowly than either. */
function chord(rate: number, frames: number): Float32Array {
	const out = new Float32Array(frames);
	for (let i = 0; i < frames; i++) {
		const t = i / rate;
		out[i] = 0.2 * Math.sin(2 * Math.PI * 261.63 * t) + 0.15 * Math.sin(2 * Math.PI * 329.63 * t + 0.4);
	}
	return out;
}

function white(frames: number, seed: number): Float32Array {
	const next = noise(seed);
	return Float32Array.from({ length: frames }, () => 0.5 * next());
}

function interleave(...channels: Float32Array[]): Float32Array {
	const frames = channels[0].length;
	const out = new Float32Array(frames * channels.length);
	for (let i = 0; i < frames; i++) {
		for (let c = 0; c < channels.length; c++) out[i * channels.length + c] = channels[c][i];
	}
	return out;
}

const ms = (rate: number, duration: number) => Math.round((rate * duration) / 1000);

function rule(name: string, about: string, rate: number, inputs: Inputs): Rule {
	return { name, about, rate, ...inputs, decision: decide(rate, inputs) };
}

/** A quantum of 128 frames, what a browser's device reads at once. */
const READ = 128;

export const rules: Rule[] = [
	rule("off-without-a-bound", "A zero bound turns the stretch off, however far the clock.", 48000, {
		lag: 4800,
		depth: 9600,
		bound: 0,
		read: READ,
	}),
	rule("on-the-clock-holds", "No lag, ample depth: play as received.", 48000, {
		lag: 0,
		depth: 9600,
		bound: 9600,
		read: READ,
	}),
	rule("half-a-hop-behind-holds", "Within half a hop of the clock is on it.", 48000, {
		lag: 240,
		depth: 9600,
		bound: 9600,
		read: READ,
	}),
	rule("half-a-hop-ahead-holds", "Within half a hop either way.", 48000, {
		lag: -240,
		depth: 9600,
		bound: 9600,
		read: READ,
	}),
	rule(
		"behind-speeds-up-in-proportion",
		"20 ms behind is 4%, splicing out between half a hop and the lag plus half a hop.",
		48000,
		{
			lag: 960,
			depth: 9600,
			bound: 9600,
			read: READ,
		},
	),
	rule("far-behind-speeds-up-at-the-cap", "100 ms behind reaches the cap at 50 ms.", 48000, {
		lag: 4800,
		depth: 9600,
		bound: 9600,
		read: READ,
	}),
	rule(
		"ahead-slows-down-in-proportion",
		"15 ms ahead is 3% slower, repeating between half a hop and the lead plus half a hop.",
		48000,
		{
			lag: -720,
			depth: 9600,
			bound: 9600,
			read: READ,
		},
	),
	rule("far-ahead-slows-down-at-the-cap", "80 ms ahead is past the cap.", 48000, {
		lag: -3840,
		depth: 9600,
		bound: 9600,
		read: READ,
	}),
	rule(
		"behind-never-speeds-into-the-cushion",
		"Behind, but with little past the cushion: the splice may take only what lies past it.",
		48000,
		{
			lag: 960,
			depth: 1400,
			bound: 9600,
			read: READ,
		},
	),
	rule(
		"about-to-run-out-bridges",
		"Less than two hops and a read received: slow down at once, up to the bound.",
		48000,
		{
			lag: 0,
			depth: 1000,
			bound: 9600,
			read: READ,
		},
	),
	rule(
		"bridges-while-behind",
		"Running out while behind still slows down, short of trailing past the bound.",
		48000,
		{
			lag: 2400,
			depth: 1000,
			bound: 9600,
			read: READ,
		},
	),
	rule(
		"past-the-bound-does-not-bridge",
		"Running out past the bound is the snap's to handle: no room to speed up either.",
		48000,
		{
			lag: 9600,
			depth: 1000,
			bound: 9600,
			read: READ,
		},
	),
	rule(
		"cushion-counts-the-read",
		"A larger device read raises the cushion: 1200 frames is under two hops and 1024.",
		48000,
		{
			lag: 0,
			depth: 1200,
			bound: 9600,
			read: 1024,
		},
	),
	rule(
		"fractional-lag-rounds-inward",
		"A playhead mid-crossfade gives a fractional lag; the range rounds toward the clock.",
		44100,
		{
			lag: 1000.6,
			depth: 4410.3,
			bound: 8820,
			read: READ,
		},
	),
	rule("8-khz-hop", "At 8 kHz a hop is 80 frames.", 8000, { lag: 100, depth: 1600, bound: 1600, read: READ }),
];

function stretch(
	name: string,
	about: string,
	rate: number,
	channels: number,
	input: Float32Array,
	decisions: Decision[],
): Case {
	// Short decimals keep the corpus small. The input is played as the floats its decimals read as, so
	// the reference and the players start from the same samples.
	const decimals = (samples: Float32Array, places: number) =>
		Array.from(samples, (sample) => Number(sample.toFixed(places)));
	const pcm = Float32Array.from(decimals(input, 6));
	const { output, shifts } = play(rate, channels, pcm, decisions);
	return {
		name,
		about,
		rate,
		channels,
		input: Array.from(pcm, (sample) => Number(sample.toFixed(6))),
		decisions,
		shifts,
		output: decimals(output, 7),
	};
}

const behind = (rate: number) =>
	decide(rate, { lag: ms(rate, 100), depth: ms(rate, 400), bound: ms(rate, 200), read: READ });
const ahead = (rate: number) =>
	decide(rate, { lag: -ms(rate, 100), depth: ms(rate, 400), bound: ms(rate, 200), read: READ });
const running = (rate: number) => decide(rate, { lag: 0, depth: ms(rate, 5), bound: ms(rate, 200), read: READ });

export const cases: Case[] = [
	stretch("hold", "Play as received.", 8000, 1, voiced(8000, ms(8000, 50), 180, 220), [HOLD]),
	stretch(
		"speed-up-voiced",
		"At the cap, the pace adds up to a hop every ten hops, and each splice skips the shift whose hop best matches the playhead's.",
		8000,
		1,
		voiced(8000, ms(8000, 250), 180, 220),
		[behind(8000)],
	),
	stretch(
		"slow-down-voiced",
		"The same ahead of the clock repeats a stretch of the history instead.",
		8000,
		1,
		voiced(8000, ms(8000, 250), 220, 180),
		[ahead(8000)],
	),
	stretch(
		"bridge",
		"Audio about to run out slows down at once, then only as fast as the cap refills its allowance.",
		8000,
		1,
		voiced(8000, ms(8000, 150), 200, 200, 3),
		[running(8000)],
	),
	stretch(
		"turns-around",
		"A change of direction starts the pace from nothing, and holding forgets it too.",
		8000,
		1,
		voiced(8000, ms(8000, 340), 150, 250, 5),
		[...Array(11).fill(behind(8000)), ...Array(11).fill(ahead(8000)), HOLD, ...Array(11).fill(behind(8000))],
	),
	stretch(
		"narrow-range",
		"A decision's range bounds the splice whatever matches better past it.",
		8000,
		1,
		voiced(8000, ms(8000, 200), 190, 210, 7),
		[{ pace: 0.1, low: 50, high: 60, urgent: false }],
	),
	stretch(
		"music",
		"A chord splices like speech: there is no other kind of content.",
		8000,
		1,
		chord(8000, ms(8000, 250)),
		[behind(8000)],
	),
	stretch("noise", "Noise splices wherever it matches best.", 8000, 1, white(ms(8000, 250), 11), [ahead(8000)]),
	stretch(
		"silence",
		"Silence matches every shift equally, so the splice takes the nearest one the range allows.",
		8000,
		1,
		new Float32Array(ms(8000, 200)),
		[behind(8000)],
	),
	stretch(
		"stereo-linked",
		"Both channels splice at the shift their samples together match best.",
		8000,
		2,
		interleave(voiced(8000, ms(8000, 200), 180, 220, 13), white(ms(8000, 200), 17)),
		[behind(8000)],
	),
	stretch(
		"stereo-opposite",
		"Channels in opposite phase, whose sum is silent, still splice at a matching period.",
		8000,
		2,
		(() => {
			const left = voiced(8000, ms(8000, 200), 100, 120, 23);
			return interleave(
				left,
				left.map((sample) => -sample),
			);
		})(),
		[behind(8000)],
	),
	stretch(
		"bridge-44100",
		"At 44.1 kHz the search strides five frames, then tries each within a stride of the best.",
		44100,
		1,
		voiced(44100, ms(44100, 40), 170, 230, 19),
		[running(44100)],
	),
	stretch(
		"bridge-48000",
		"At 48 kHz the stride is six frames.",
		48000,
		1,
		voiced(48000, ms(48000, 40), 210, 190, 23),
		[running(48000)],
	),
	stretch(
		"speed-up-16000",
		"Speeding up strides the search too, nearest the playhead first: two frames at 16 kHz.",
		16000,
		1,
		voiced(16000, ms(16000, 220), 160, 240, 29),
		[behind(16000)],
	),
];

if (import.meta.main) {
	const here = dirname(fileURLToPath(import.meta.url));
	writeFileSync(join(here, "decisions.json"), `${JSON.stringify(rules)}\n`);
	writeFileSync(join(here, "corpus.json"), `${JSON.stringify(cases)}\n`);
	console.log(`wrote ${rules.length} rules and ${cases.length} cases`);
}
