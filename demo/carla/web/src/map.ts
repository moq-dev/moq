/**
 * The fleet map: baked CARLA road geometry, and the projection that puts truck
 * poses on top of it.
 *
 * Everything here is in CARLA world METRES (x east, y south), which is also what
 * the trucks put on their `status` track, so roads and markers need no common
 * reference beyond this module. Screen y grows downward like CARLA's y, so the
 * projection is a scale with no flip, and a yaw in degrees is a CSS rotation.
 *
 * The view is always widened to the container's exact aspect ratio rather than
 * letterboxed by the SVG, because the markers are HTML positioned in percent:
 * the two only line up if the viewBox and the box agree.
 *
 * @module
 */

/** One baked town, written by the simulator's `moq_carla_sim.roadmap`. */
export interface RoadMap {
	name: string;
	/** [minX, minY, maxX, maxY], metres. */
	bbox: [number, number, number, number];
	features: { geometry: { coordinates: [number, number][][] } }[];
	/** Calibrated top-down image in the same world metres as the roads. */
	aerial?: { image: string; bbox: [number, number, number, number] };
}

/** A rectangle of CARLA world metres, as drawn. */
export interface View {
	minX: number;
	minY: number;
	maxX: number;
	maxY: number;
}

/** Something with a position on the map. */
export interface Located {
	x: number;
	y: number;
}

/** Metres across the smallest auto-fitted view, so a convoy is not a wall of truck. */
const MIN_SPAN = 250;

/** Fraction of the span left empty around auto-fitted trucks. */
const PADDING = 0.2;

/**
 * Metres, the narrowest a baked town may be drawn on either axis.
 *
 * A town whose lanes are all collinear - one straight road, which is what a
 * hand-authored OpenDRIVE tends to look like - bakes to a bbox with zero span on
 * an axis. Left alone that reaches the SVG as a zero viewBox dimension and makes
 * the scale bar's fraction non-finite.
 */
const MIN_MAP_SPAN = 10;

/** The rectangle a baked town covers, never thinner than it can be drawn. */
export function mapView(map: RoadMap, padding = 0): View {
	const [leftX, topY, rightX, bottomY] = map.bbox;
	const aerial = map.aerial?.bbox ?? map.bbox;
	const minX = Math.min(leftX, aerial[0]);
	const minY = Math.min(topY, aerial[1]);
	const maxX = Math.max(rightX, aerial[2]);
	const maxY = Math.max(bottomY, aerial[3]);
	const [left, right] = widen(minX, maxX, MIN_MAP_SPAN);
	const [top, bottom] = widen(minY, maxY, MIN_MAP_SPAN);
	const margin = Math.max(right - left, bottom - top) * padding;
	return { minX: left - margin, maxX: right + margin, minY: top - margin, maxY: bottom + margin };
}

/** Grow an axis around its centre until it spans at least `span`. */
function widen(low: number, high: number, span: number): [number, number] {
	if (high - low >= span) return [low, high];
	const centre = (low + high) / 2;
	return [centre - span / 2, centre + span / 2];
}

/**
 * The town the map shows when a relay carries trucks from more than one.
 *
 * CARLA coordinates from different towns are unrelated, so the map can only be
 * one of them: the town most trucks are in, ties broken by name so the choice is
 * stable rather than a function of announcement order. Everyone else waits
 * beside the map, the same place a truck with no position goes.
 */
export function fleetTown(towns: Iterable<string>): string | undefined {
	const counts = new Map<string, number>();
	for (const town of towns) counts.set(town, (counts.get(town) ?? 0) + 1);

	let best: string | undefined;
	let most = 0;
	for (const [town, count] of [...counts].sort(([a], [b]) => (a < b ? -1 : 1))) {
		if (count > most) {
			most = count;
			best = town;
		}
	}
	return best;
}

/** A square view around whatever positions we have, until a town is baked. */
export function fitPoints(points: readonly Located[]): View | undefined {
	if (points.length === 0) return undefined;
	let minX = Infinity;
	let minY = Infinity;
	let maxX = -Infinity;
	let maxY = -Infinity;
	for (const { x, y } of points) {
		minX = Math.min(minX, x);
		maxX = Math.max(maxX, x);
		minY = Math.min(minY, y);
		maxY = Math.max(maxY, y);
	}
	// Square and floored: a convoy sits nose to tail, and fitting tightly to
	// three trucks 12 m apart would zoom the map to a stretch of one road.
	const span = Math.max(maxX - minX, maxY - minY, MIN_SPAN) * (1 + PADDING);
	const x = (minX + maxX) / 2;
	const y = (minY + maxY) / 2;
	return { minX: x - span / 2, maxX: x + span / 2, minY: y - span / 2, maxY: y + span / 2 };
}

/**
 * Whether an auto-fitted view has to be recomputed for these positions.
 *
 * Refitting on every pose would slide the whole map under the driver at 10 Hz.
 * Instead the view holds still until a truck reaches `margin` of the edge.
 */
export function needsRefit(view: View | undefined, points: readonly Located[], margin = 0.08): boolean {
	if (!view) return points.length > 0;
	if (points.length === 0) return false;
	const insetX = (view.maxX - view.minX) * margin;
	const insetY = (view.maxY - view.minY) * margin;
	return points.some(
		({ x, y }) =>
			x < view.minX + insetX || x > view.maxX - insetX || y < view.minY + insetY || y > view.maxY - insetY,
	);
}

/** Widen the shorter axis until the view matches the container it is drawn in. */
export function expandToAspect(view: View, aspect: number): View {
	const width = view.maxX - view.minX;
	const height = view.maxY - view.minY;
	if (!Number.isFinite(aspect) || aspect <= 0 || width <= 0 || height <= 0) return view;
	const x = (view.minX + view.maxX) / 2;
	const y = (view.minY + view.maxY) / 2;
	const spanX = width / height < aspect ? height * aspect : width;
	const spanY = width / height < aspect ? height : width / aspect;
	return { minX: x - spanX / 2, maxX: x + spanX / 2, minY: y - spanY / 2, maxY: y + spanY / 2 };
}

/** Where a position sits in the view, as CSS percentages. */
export function project(view: View, point: Located): { left: number; top: number } {
	const width = view.maxX - view.minX;
	const height = view.maxY - view.minY;
	return {
		left: width > 0 ? ((point.x - view.minX) / width) * 100 : 50,
		top: height > 0 ? ((point.y - view.minY) / height) * 100 : 50,
	};
}

/** The SVG viewBox covering a view, in metres. */
export function viewBox(view: View): string {
	return `${view.minX} ${view.minY} ${view.maxX - view.minX} ${view.maxY - view.minY}`;
}

/** Every lane in one SVG path, in metres. */
export function pathData(map: RoadMap): string {
	const parts: string[] = [];
	for (const feature of map.features) {
		for (const line of feature.geometry.coordinates) {
			if (line.length < 2) continue;
			parts.push(line.map(([x, y], index) => `${index ? "L" : "M"}${x} ${y}`).join(" "));
		}
	}
	return parts.join(" ");
}

/**
 * A round scale-bar length and the fraction of the view's width it covers.
 *
 * Round means 1, 2, or 5 times a power of ten, the largest that still fits in a
 * quarter of the view, so the bar stays readable at every zoom.
 */
export function scaleBar(view: View, fraction = 0.25): { metres: number; fraction: number } {
	const width = view.maxX - view.minX;
	const budget = width * fraction;
	const magnitude = 10 ** Math.floor(Math.log10(budget));
	const metres = [5, 2, 1].map((step) => step * magnitude).find((length) => length <= budget) ?? magnitude;
	return { metres, fraction: metres / width };
}

/** A truck's speed as the map labels it. */
export function speedLabel(metresPerSecond: number): string {
	return `${Math.round(metresPerSecond * 3.6)} km/h`;
}
