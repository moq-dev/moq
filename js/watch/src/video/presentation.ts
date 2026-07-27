import type * as Catalog from "@moq/hang/catalog";

type VideoRotation = 0 | 90 | 180 | 270;

type VideoDimensions = {
	width: number;
	height: number;
};

/** Canvas transform and source dimensions for a video presentation. */
export type CanvasPresentationTransform = {
	matrix: [number, number, number, number, number, number];
	source: VideoDimensions;
};

function normalizeVideoRotation(rotation = 0): VideoRotation {
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

function flipMatrix(
	matrix: [number, number, number, number, number, number],
	width: number,
): [number, number, number, number, number, number] {
	const [a, b, c, d, e, f] = matrix;
	return [clean(-a), clean(b), clean(-c), clean(d), clean(width - e), clean(f)];
}

/** Return the canvas transform needed to render a catalog video presentation. */
export function canvasPresentationTransform(
	output: VideoDimensions,
	video?: Catalog.Video,
): CanvasPresentationTransform {
	const rotation = normalizeVideoRotation(video?.rotation);
	const source = rotateVideoDimensions(output, rotation);

	let matrix: [number, number, number, number, number, number];
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

	if (video?.flip) matrix = flipMatrix(matrix, output.width);
	return { matrix, source };
}
