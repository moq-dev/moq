/** The wire transport a session runs over. */
export type Transport = "webtransport" | "websocket" | "webrtc";

/**
 * The transport a live session is actually running over. WebSocket and WebRTC adapters
 * share the WebTransport shape and carry their label, so `@moq/qmux` stays a lazy chunk.
 *
 * @internal Not re-exported from the package entrypoint.
 */
export function transportOf(quic: WebTransport): Transport {
	const label = "transport" in quic ? quic.transport : undefined;
	return label === "webrtc" || label === "websocket" ? label : "webtransport";
}
