/** Build-time map assets share the world-coordinate projection used by telemetry. */
import type { RoadMap } from "../map.ts";

interface AerialManifest {
	name: string;
	image: string;
	bbox: [number, number, number, number];
}

const BAKED = import.meta.glob<RoadMap>(["./*.json", "!./*.aerial.json"], { import: "default" });
const AERIALS = import.meta.glob<AerialManifest>("./*.aerial.json", { import: "default" });
const IMAGES = import.meta.glob<string>(["./*.aerial.png", "./*.aerial.webp"], {
	query: "?url",
	import: "default",
	eager: true,
});

/** Unknown towns fall back to positions; an unreadable aerial keeps the roads. */
export async function loadRoadMap(name: string): Promise<RoadMap | undefined> {
	const load = BAKED[`./${name}.json`];
	if (!load) return undefined;
	const map = await load();
	const aerial = AERIALS[`./${name}.aerial.json`];
	if (!aerial) return map;
	const manifest = await aerial();
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
