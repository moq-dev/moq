type VideoRotation = 0 | 90 | 180 | 270;

/** Pixel dimensions used by the publisher's presentation metadata. */
export type VideoDimensions = {
	width: number;
	height: number;
};

/** Normalize clockwise degrees to the nearest quarter-turn. */
export function normalizeVideoRotation(rotation = 0): VideoRotation {
	if (!Number.isFinite(rotation)) throw new RangeError("video rotation must be finite");

	const normalized = ((rotation % 360) + 360) % 360;
	return ((Math.round(normalized / 90) % 4) * 90) as VideoRotation;
}

/** Return dimensions after applying a clockwise quarter-turn. */
export function rotateVideoDimensions(dimensions: VideoDimensions, rotation = 0): VideoDimensions {
	const normalized = normalizeVideoRotation(rotation);
	return normalized === 90 || normalized === 270
		? { width: dimensions.height, height: dimensions.width }
		: { width: dimensions.width, height: dimensions.height };
}

function clean(value: number): number {
	return Object.is(value, -0) ? 0 : value;
}

/** Return the Canvas matrix for final output dimensions after rotation and an optional horizontal flip. */
export function canvasPresentationTransform(
	output: VideoDimensions,
	rotation = 0,
	flip = false,
): [number, number, number, number, number, number] {
	let matrix: [number, number, number, number, number, number];
	switch (normalizeVideoRotation(rotation)) {
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
	}

	if (!flip) return matrix;
	const [a, b, c, d, e, f] = matrix;
	return [clean(-a), clean(b), clean(-c), clean(d), clean(output.width - e), clean(f)];
}
