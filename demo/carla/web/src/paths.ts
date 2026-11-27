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

/** Where every camera broadcast lies; {@link parseCamera} picks the cameras out of what is listed. */
export const CAMERAS = Moq.Path.from("carla");

/** One announced camera. */
export interface Camera {
	/** The truck publishing it, which is also its control scope. */
	truck: string;
	/** Stable per-camera id, e.g. "truck1/front". */
	id: string;
}

/** Parse a camera broadcast path, or `undefined` for anything else. */
export function parseCamera(path: string): Camera | undefined {
	const [root, truck, file, ...rest] = path.split("/");
	if (root !== CAMERAS || !truck || rest.length > 0 || !file?.endsWith(".hang")) return undefined;
	return { truck, id: `${truck}/${file.slice(0, -".hang".length)}` };
}

/** Where one driver of a truck publishes its control broadcast. */
export function driverPath(truck: string, driver: string): Moq.Path.Valid {
	return Moq.Path.from(`viewer/carla/${truck}/${driver}`);
}

/** A fresh driver id, public since the truck's status names its driver. */
export function driverId(): string {
	return crypto.randomUUID().replaceAll("-", "").slice(0, 12);
}
