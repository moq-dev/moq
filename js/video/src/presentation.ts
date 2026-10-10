/** How a frame is oriented on screen: a clockwise rotation, then an optional horizontal mirror. */
export type Presentation = {
	/** Clockwise rotation in degrees, rounded to the nearest quarter-turn. Defaults to 0. */
	rotation?: number;
	/** Mirror the rotated picture horizontally. Defaults to false. */
	flip?: boolean;
};

/** A width and height in pixels. */
export type Dimensions = {
	width: number;
	height: number;
};

type Rotation = 0 | 90 | 180 | 270;
type Matrix = [number, number, number, number, number, number];

/** Canvas transform and source dimensions for a video presentation. */
export type CanvasPresentationTransform = {
	matrix: Matrix;
	source: Dimensions;
};

function normalizeRotation(rotation = 0): Rotation {
	const normalized = ((rotation % 360) + 360) % 360;
	return ((Math.round(normalized / 90) % 4) * 90) as Rotation;
}

/** Return dimensions after applying a clockwise quarter-turn. */
export function rotateVideoDimensions(dimensions: Dimensions, rotation = 0): Dimensions {
	const normalized = normalizeRotation(rotation);
	return normalized === 90 || normalized === 270
		? { width: dimensions.height, height: dimensions.width }
		: { width: dimensions.width, height: dimensions.height };
}

function clean(value: number): number {
	return Object.is(value, -0) ? 0 : value;
}

function flipMatrix(matrix: Matrix, width: number): Matrix {
	const [a, b, c, d, e, f] = matrix;
	return [clean(-a), clean(b), clean(-c), clean(d), clean(width - e), clean(f)];
}

/**
 * Return the Canvas2D transform that draws a frame, stretched to `source`, onto an `output` canvas.
 *
 * `matrix` is the `setTransform(a, b, c, d, e, f)` argument list.
 */
export function canvasPresentationTransform(
	output: Dimensions,
	presentation?: Presentation,
): CanvasPresentationTransform {
	const rotation = normalizeRotation(presentation?.rotation);
	const source = rotateVideoDimensions(output, rotation);

	let matrix: Matrix;
	switch (rotation) {
		case 90:
			matrix = [0, 1, -1, 0, output.width, 0];
			break;
		case 180:
			matrix = [-1, 0, 0, -1, output.width, output.height];
			break;
		case 270:
			matrix = [0, -1, 1, 0, 0, output.height];
			break;
		default:
			matrix = [1, 0, 0, 1, 0, 0];
			break;
	}

	if (presentation?.flip) matrix = flipMatrix(matrix, output.width);
	return { matrix, source };
}

/**
 * Return the affine map from a canvas texture coordinate to the frame texture coordinate it shows.
 *
 * The WebGPU path samples the frame per output pixel, so it needs the inverse of the Canvas2D
 * transform, normalized to 0..1 on both sides. The result is two rows: `u' = x · (u, v, 1)` and
 * `v' = y · (u, v, 1)`. Deriving it from {@link canvasPresentationTransform} keeps both paths in
 * lockstep.
 */
export function texturePresentationTransform(
	output: Dimensions,
	presentation?: Presentation,
): { x: [number, number, number]; y: [number, number, number] } {
	const { matrix, source } = canvasPresentationTransform(output, presentation);
	const [a, b, c, d, e, f] = matrix;
	const det = a * d - b * c;
	const sx = det * source.width;
	const sy = det * source.height;
	return {
		x: [clean((d * output.width) / sx), clean((-c * output.height) / sx), clean((c * f - d * e) / sx)],
		y: [clean((-b * output.width) / sy), clean((a * output.height) / sy), clean((b * e - a * f) / sy)],
	};
}
