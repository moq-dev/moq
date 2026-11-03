/**
 * The broadcast paths shared with the simulator.
 *
 * Trucks publish one media broadcast per camera at `carla/{truck}/{camera}.hang`.
 * Drivers publish their `control` track at `viewer/carla/{truck}/{driver}`, a
 * prefix the demo relay lets anonymous viewers publish under.
 *
 * @module
 */
import * as Moq from "@moq/net";

/** Every camera broadcast, and nothing else that shares the relay. */
export const CAMERAS = Moq.Path.Pattern.parse("carla/*/*.hang");

/** One announced camera. */
export interface Camera {
	/** The truck publishing it, which is also its control scope. */
	truck: string;
	/** Stable per-camera id, e.g. "truck1/front". */
	id: string;
}

/** Parse a camera broadcast path, or `undefined` for anything else. */
export function parseCamera(path: string): Camera | undefined {
	if (!CAMERAS.matches(path)) return undefined;
	const [, truck, file] = path.split("/");
	return { truck, id: `${truck}/${file.slice(0, -".hang".length)}` };
}

/** Where one driver of a truck publishes its control broadcast. */
export function driverPath(truck: string, driver: string): Moq.Path.Valid {
	return Moq.Path.from(`viewer/carla/${truck}/${driver}`);
}
