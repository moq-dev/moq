/** The wire transport a session runs over. */
export type Transport = "webtransport" | "websocket";

/**
 * The transport a live session is actually running over. The WebSocket adapter shares the
 * WebTransport shape and carries its label, so `@moq/qmux` stays a lazy chunk.
 *
 * @internal Not re-exported from the package entrypoint.
 */
export function transportOf(quic: WebTransport): Transport {
	const label = "transport" in quic ? quic.transport : undefined;
	return label === "websocket" ? label : "webtransport";
}
