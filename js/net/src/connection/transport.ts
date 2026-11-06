import Session from "@moq/qmux";

/** The wire transport a session runs over. */
export type Transport = "webtransport" | "websocket" | "webrtc";

/**
 * The transport a live session is actually running over. WebSocket and WebRTC adapters
 * share the WebTransport shape; WebRTC carries its label to avoid a dependency cycle.
 *
 * @internal Not re-exported from the package entrypoint.
 */
export function transportOf(quic: WebTransport): Transport {
	if ("transport" in quic && quic.transport === "webrtc") return "webrtc";
	return quic instanceof Session ? "websocket" : "webtransport";
}
