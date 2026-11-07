/**
 * The paths and connection settings shared with the simulator.
 *
 * Trucks publish one media broadcast per camera at `carla/{truck}/{camera}.hang`,
 * and accept drivers on a socket endpoint named for the truck. A driver dials it
 * from an endpoint of its own, `driver-{id}`, a form the demo relay lets
 * anonymous viewers publish. The truck's public `status` names a driver by its
 * session's {@link handle} instead, so no viewer learns an id it could announce.
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

/** The prefix every truck and driver socket endpoint sits under, as `carla/.socket/{id}`. */
export const SOCKET = Moq.Path.from("carla");

/**
 * The versions offered to the relay, most preferred first: the ones that carry socket
 * endpoints, which are hidden broadcasts that moq-lite before 07 never carries.
 */
export const VERSIONS = ["moq-lite-07-wip", "moq-transport-22"] as const;

function hex(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** A fresh driver endpoint id: random, so no other viewer can claim it. */
export function driverId(): string {
	return `driver-${hex(crypto.getRandomValues(new Uint8Array(16)))}`;
}

/**
 * The name the truck's `status` gives the lease or e-stop a session carried, as the worker's `drive.handle` derives it.
 *
 * A hash of the session id, which only the session's two ends know.
 */
export async function handle(session: string): Promise<string> {
	const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(session));
	return `driver-${hex(new Uint8Array(digest, 0, 4))}`;
}
