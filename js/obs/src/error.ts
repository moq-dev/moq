/**
 * Why {@link connect} could not reach OBS:
 *
 * - `denied`: the browser was not allowed to reach apps on this device: the user said no or left its prompt unanswered.
 * - `unreachable`: nothing answered on the port, such as OBS not running or its WebSocket server off.
 * - `refused`: OBS rejected the password, or needs one and none was given.
 * - `blocked`: the browser refused to open the socket at all, as some do for `ws://` from an HTTPS page.
 * - `unsupported`: the server is not obs-websocket 5 (OBS 28 or newer).
 */
export type ConnectReason = "denied" | "unreachable" | "refused" | "blocked" | "unsupported";

/** The connection to OBS failed before it was ready, for the {@link ConnectReason} in `reason`. */
export class ConnectError extends Error {
	/** Why the connection failed. */
	readonly reason: ConnectReason;

	constructor(reason: ConnectReason, message: string) {
		super(message);
		this.name = "ConnectError";
		this.reason = reason;
	}
}

/** OBS answered a request with a failure status. */
export class RequestError extends Error {
	/** The obs-websocket request type, such as `StartStream`. */
	readonly request: string;
	/** The obs-websocket `RequestStatus` code. */
	readonly code: number;

	constructor(request: string, code: number, comment: string | undefined) {
		super(`OBS refused ${request} (code ${code})${comment ? `: ${comment}` : ""}`);
		this.name = "RequestError";
		this.request = request;
		this.code = code;
	}
}
