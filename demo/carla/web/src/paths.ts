/**
 * The paths and connection settings shared with the simulator.
 *
 * Trucks publish one media broadcast per camera at `carla/{truck}/{camera}.hang`,
 * and accept drivers on a server socket endpoint named for the truck. A driver
 * dials it from a client endpoint of its own, the only role the demo relay lets
 * anonymous viewers publish.
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

/** The prefix every truck and driver socket endpoint sits under: `carla/.server/{truck}` and `carla/.client/{driver}`. */
export const SOCKET = Moq.Path.from("carla");

/**
 * The versions offered to the relay, most preferred first: the ones that carry socket
 * endpoints, which are hidden broadcasts that moq-lite before 07 never carries.
 */
export const VERSIONS = ["moq-lite-07-wip", "moq-transport-22"] as const;

/** A fresh driver endpoint id. Public, since the truck's status names its driver. */
export function driverId(): string {
	return `driver-${crypto.randomUUID().replaceAll("-", "").slice(0, 12)}`;
}
