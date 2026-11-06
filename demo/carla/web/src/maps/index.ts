/** Build-time map assets share the world-coordinate projection used by telemetry. */
import type { RoadMap } from "../map.ts";

interface AerialManifest {
	name: string;
	image: string;
	bbox: [number, number, number, number];
}

const BAKED = import.meta.glob<RoadMap>(["./*.json", "!./*.aerial.json"], { import: "default", eager: true });
const AERIALS = import.meta.glob<AerialManifest>("./*.aerial.json", { import: "default", eager: true });
const IMAGES = import.meta.glob<string>(["./*.aerial.png", "./*.aerial.webp"], {
	query: "?url",
	import: "default",
	eager: true,
});

/**
 * Start fetching every aerial before any truck names its town, so the map can
 * appear with the first truck rather than a download later.
 */
export function preloadAerials() {
	for (const image of Object.values(IMAGES)) new Image().src = image;
}

/** Unknown towns fall back to positions; an unreadable aerial keeps the roads. */
export async function loadRoadMap(name: string): Promise<RoadMap | undefined> {
	const map = BAKED[`./${name}.json`];
	if (!map) return undefined;
	const manifest = AERIALS[`./${name}.aerial.json`];
	if (!manifest) return map;
	const image = IMAGES[`./${manifest.image}`];
	const bbox = manifest.bbox;
	if (
		manifest.name !== name ||
		!image ||
		bbox.length !== 4 ||
		!bbox.every(Number.isFinite) ||
		bbox[2] <= bbox[0] ||
		bbox[3] <= bbox[1]
	) {
		throw new Error(`Invalid aerial manifest for ${name}`);
	}
	try {
		// Decode before adopting the layer: a failed asset must leave a usable road map.
		const bitmap = new Image();
		bitmap.src = image;
		await bitmap.decode();
		return { ...map, aerial: { image, bbox } };
	} catch (error) {
		console.warn(`Aerial image for ${name} unavailable; using roads`, error);
		return map;
	}
}
