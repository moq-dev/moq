import Session from "@moq/qmux";

/** The wire transport a session runs over. */
export type Transport = "webtransport" | "websocket";

/**
 * The transport a live session is actually running over. qmux implements the `WebTransport`
 * interface on top of a WebSocket, so the two are indistinguishable by shape.
 *
 * @internal Not re-exported from the package entrypoint.
 */
export function transportOf(quic: WebTransport): Transport {
	return quic instanceof Session ? "websocket" : "webtransport";
}
