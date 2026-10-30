/**
 * The media fixture's pattern as ffmpeg filters, for a native publisher.
 *
 * The browser fixture paints {@link Pattern.paint} on a canvas and steps an oscillator through
 * {@link Pattern.stepFrequency}. A native publisher has neither, so this states the same picture as
 * `drawbox` filters over a gray `color` source and the same tone as an `aevalsrc` expression, both
 * derived from the pattern's constants so the two cannot drift apart. Both run off ffmpeg's own
 * timestamps, starting at zero, so the media itself carries the sync the player is graded on.
 *
 * Free of browser imports: interop.sh runs it under Bun to print the filters.
 *
 *     bun src/native.ts video
 *     bun src/native.ts audio
 *
 * @module
 */
import * as Pattern from "./pattern";

/** Peak amplitude of the tone, the browser fixture's `AMPLITUDE`. */
const AMPLITUDE = 0.5;

// Mirrors pattern.ts: the strip is the top sixth, the bar an eighth of the width.
const STRIP = Math.round(Pattern.HEIGHT / 6);
const BAR = Pattern.WIDTH / 8;

// Pixel bounds of a cell. drawbox takes whole pixels, and decode only samples each cell's center.
function cell(i: number): { x: number; w: number } {
	const x = Math.floor((i * Pattern.WIDTH) / Pattern.CELLS);
	return { x, w: Math.floor(((i + 1) * Pattern.WIDTH) / Pattern.CELLS) - x };
}

// One counter bit of the frame number, as an ffmpeg expression over `n`.
const bit = (i: number) => `mod(floor(n/${2 ** i}),2)`;

/**
 * The picture as a lavfi graph: a gray frame with the counter strip and the sweeping bar, where the
 * counter is ffmpeg's frame number. Generation is fixed per publisher, so it is painted as constant
 * blocks, and the parity block counts it too.
 */
export function video(generation = 0): string {
	const box = (x: number, y: number, w: number, h: number, color: string, enable?: string) =>
		`drawbox=x=${x}:y=${y}:w=${w}:h=${h}:c=${color}:t=fill${enable ? `:enable='${enable}'` : ""}`;

	// The sweeping bar first, so the strip covers its top. drawbox cannot read the frame number in
	// its position, so the bar is a second source overlaid where it can.
	const source = (color: string, w: number, h: number) => `color=c=${color}:s=${w}x${h}:r=${Pattern.FPS}`;
	const filters = [
		`${source("0x808080", Pattern.WIDTH, Pattern.HEIGHT)}[bg]`,
		`${source("white", BAR, Pattern.HEIGHT - STRIP)}[bar]`,
		`[bg][bar]overlay=x='mod(n*4,${Pattern.WIDTH})-${BAR / 2}':y=${STRIP}:eval=frame`,
	];

	// Every block black, then the white reference and each set bit painted over it.
	const strip = cell(1);
	filters.push(box(strip.x, 0, Pattern.WIDTH - strip.x, STRIP, "black"));
	const white = cell(0);
	filters.push(box(white.x, 0, white.w, STRIP, "white"));

	const bits: string[] = [];
	for (let i = 0; i < Pattern.FRAME_BITS; i++) {
		const { x, w } = cell(i + 2);
		filters.push(box(x, 0, w, STRIP, "white", bit(i)));
		bits.push(bit(i));
	}

	const generationBits = Pattern.CELLS - Pattern.FRAME_BITS - 3;
	let generationParity = 0;
	for (let i = 0; i < generationBits; i++) {
		if (!((generation >> i) & 1)) continue;
		const { x, w } = cell(Pattern.FRAME_BITS + 2 + i);
		filters.push(box(x, 0, w, STRIP, "white"));
		generationParity ^= 1;
	}

	const parity = cell(Pattern.CELLS - 1);
	filters.push(box(parity.x, 0, parity.w, STRIP, "white", `mod(${bits.join("+")}+${generationParity},2)`));

	return `${filters[0]};${filters[1]};${filters.slice(2).join(",")}`;
}

/** The tone as a lavfi source: the step table at the fixture's rate, one channel. */
export function audio(sampleRate = 48_000): string {
	const step = `mod(floor(t/${Pattern.STEP_MS / 1000}),${Pattern.STEPS})`;
	const hz = `(${Pattern.STEP_BASE_HZ}+${Pattern.STEP_GAP_HZ}*${step})`;
	// Every step lasts a whole number of its own cycles, so the phase is continuous at each boundary
	// without integrating the frequency.
	return `aevalsrc='${AMPLITUDE}*sin(2*PI*${hz}*t)':s=${sampleRate}:c=mono`;
}

if (import.meta.main) {
	const what = process.argv[2];
	if (what === "video") console.log(video());
	else if (what === "audio") console.log(audio());
	else {
		console.error("usage: native.ts video|audio");
		process.exit(2);
	}
}
