/**
 * The deterministic media fixture's self-describing pattern.
 *
 * Video carries a frame counter painted as black/white blocks along the top strip, so a subscriber
 * can name the exact frame it is presenting from the pixels alone. Audio carries a continuous sine
 * whose frequency steps through a fixed table, so a subscriber can name the exact audio step it is
 * hearing from a spectrum alone. Both are driven by one clock at the publisher, which makes the two
 * readings comparable: the step the audio is playing must match the step the painted frame belongs
 * to, and the gap between them is the synchronization error.
 *
 * @module
 */

/** Frames per second the fixture paints, captures, and encodes. */
export const FPS = 30;

/** Coded size of the fixture canvas. A multiple of 16 so the encoder does not rescale it. */
export const WIDTH = 320;
/** Coded height of the fixture canvas. See {@link WIDTH}. */
export const HEIGHT = 240;

/**
 * Bits of frame counter painted into each frame.
 *
 * 13 bits wraps after 8192 frames, about four and a half minutes, which is longer than any
 * measurement window: a wrap inside one would read as the picture jumping backwards.
 */
export const FRAME_BITS = 13;

/**
 * Blocks along the top strip: a white reference, a black reference, one per counter bit, and an
 * even parity block.
 *
 * Parity is what separates "this frame is not the fixture" from "one block was misread". Without
 * it a single flipped bit reads as a wild frame number, which looks exactly like the picture
 * jumping backwards.
 */
export const CELLS = FRAME_BITS + 3;

/** Fraction of the frame height occupied by the counter strip. */
const STRIP = 1 / 6;

/** Fraction of a block sampled when decoding, centered, so encoder ringing at the edges is ignored. */
const SAMPLE = 0.5;

/** Minimum gap between the white and black reference blocks for a frame to be readable, 0-255. */
export const MIN_CONTRAST = 60;

/** How long each audio tone step lasts. */
export const STEP_MS = 200;

/** Tone steps before the table repeats. The cycle (STEPS * STEP_MS) bounds measurable skew. */
export const STEPS = 16;

/** Frequency of tone step 0. */
export const STEP_BASE_HZ = 500;

/** Frequency gap between adjacent tone steps. Wide enough to separate under Opus and an FFT bin. */
export const STEP_GAP_HZ = 140;

/** The tone frequency for a step index, which wraps at {@link STEPS}. */
export function stepFrequency(step: number): number {
	return STEP_BASE_HZ + (step % STEPS) * STEP_GAP_HZ;
}

/**
 * The band the tone table occupies, with half a step of margin on each side.
 *
 * Stated rather than derived from `stepFrequency(STEPS)`, which wraps back to the base and would
 * narrow the search to step 0 alone.
 */
export const BAND = {
	lowHz: STEP_BASE_HZ - STEP_GAP_HZ / 2,
	highHz: STEP_BASE_HZ + (STEPS - 0.5) * STEP_GAP_HZ,
};

/** The tone step a painted frame belongs to, i.e. what a synchronized subscriber must be hearing. */
export function expectedStep(frameId: number): number {
	return Math.floor((frameId * 1000) / FPS / STEP_MS) % STEPS;
}

/** The nearest tone step to a measured frequency, or undefined when it falls outside the table. */
export function nearestStep(hz: number): number | undefined {
	const step = Math.round((hz - STEP_BASE_HZ) / STEP_GAP_HZ);
	if (step < 0 || step >= STEPS) return undefined;
	if (Math.abs(hz - stepFrequency(step)) > STEP_GAP_HZ / 2) return undefined;
	return step;
}

/**
 * Signed distance from `expected` to `actual` around the step table, in steps.
 *
 * The table wraps, so a raw subtraction would report a one-step lag as `STEPS - 1`. The result is
 * in `[-STEPS/2, STEPS/2)`: negative means the audio is behind the video.
 */
export function stepSkew(actual: number, expected: number): number {
	const diff = (((actual - expected) % STEPS) + STEPS) % STEPS;
	return diff >= STEPS / 2 ? diff - STEPS : diff;
}

// Even parity over the painted counter bits.
function parity(frameId: number): number {
	let bit = 0;
	for (let i = 0; i < FRAME_BITS; i++) bit ^= (frameId >> i) & 1;
	return bit;
}

/** Paint one fixture frame: the counter strip on top, a sweeping bar below. */
export function paint(ctx: CanvasRenderingContext2D, frameId: number): void {
	const width = ctx.canvas.width;
	const height = ctx.canvas.height;
	const strip = height * STRIP;
	const cell = width / CELLS;

	// Mid gray, so a decoder that emits an empty (black) frame cannot pass the reference check.
	ctx.fillStyle = "#808080";
	ctx.fillRect(0, 0, width, height);

	// Cell 0 is the white reference, cell 1 the black reference, then the counter LSB first, then parity.
	const bits = [1, 0];
	for (let i = 0; i < FRAME_BITS; i++) bits.push((frameId >> i) & 1);
	bits.push(parity(frameId));

	for (let i = 0; i < CELLS; i++) {
		ctx.fillStyle = bits[i] ? "#ffffff" : "#000000";
		ctx.fillRect(i * cell, 0, cell, strip);
	}

	// A bar sweeping with the counter. Purely a function of frameId, so a frozen counter also
	// freezes the picture and the encoder has nothing left to signal progress with.
	const bar = width / 8;
	ctx.fillStyle = "#ffffff";
	ctx.fillRect(((frameId * 4) % width) - bar / 2, strip, bar, height - strip);
}

/** What {@link decode} read out of a painted frame. */
export type Decoded = {
	/** The painted frame counter. */
	frameId: number;
	/** Gap between the white and black reference blocks, 0-255. Below {@link MIN_CONTRAST} is unreadable. */
	contrast: number;
};

// Average the luma of a block's center. The pattern is grayscale, so the red channel is the luma.
function blockLuma(pixels: Uint8ClampedArray, width: number, x0: number, y0: number, x1: number, y1: number): number {
	let sum = 0;
	let count = 0;
	for (let y = Math.floor(y0); y < Math.ceil(y1); y++) {
		for (let x = Math.floor(x0); x < Math.ceil(x1); x++) {
			sum += pixels[(y * width + x) * 4];
			count++;
		}
	}
	return count > 0 ? sum / count : 0;
}

/**
 * Read the frame counter back out of a presented frame.
 *
 * Returns undefined when the reference blocks are too close together to threshold against, which is
 * what a blank canvas, a letterboxed frame, or a picture that is not the fixture looks like.
 */
export function decode(pixels: Uint8ClampedArray, width: number, height: number): Decoded | undefined {
	if (width < CELLS * 4 || height < 8) return undefined;

	const cell = width / CELLS;
	const strip = height * STRIP;
	const padX = (cell * (1 - SAMPLE)) / 2;
	const padY = (strip * (1 - SAMPLE)) / 2;

	const luma = (i: number) => blockLuma(pixels, width, i * cell + padX, padY, (i + 1) * cell - padX, strip - padY);

	const white = luma(0);
	const black = luma(1);
	const contrast = white - black;
	if (contrast < MIN_CONTRAST) return undefined;

	const threshold = (white + black) / 2;
	let frameId = 0;
	for (let i = 0; i < FRAME_BITS; i++) {
		if (luma(i + 2) > threshold) frameId |= 1 << i;
	}

	// A misread block is not a frame number, so report it as unreadable rather than as a jump.
	if ((luma(CELLS - 1) > threshold ? 1 : 0) !== parity(frameId)) return undefined;

	return { frameId, contrast };
}
