import type * as Moq from "@moq/net";

/** What the map has to show: vehicles, or why there are none. */
export type Presence = "online" | "offline" | "connecting";

/**
 * Whether the simulator is publishing, judged from the relay session and the
 * vehicles announced on it.
 *
 * Only a connected session can say nothing is announced: before that, an empty
 * map means the relay has not answered yet.
 */
export function presence(connection: Moq.Connection.Status, vehicles: number): Presence {
	if (vehicles > 0) return "online";
	return connection === "connected" ? "offline" : "connecting";
}
