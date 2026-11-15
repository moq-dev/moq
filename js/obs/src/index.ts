/**
 * Set up the OBS Studio running on the user's own machine from a web page, through OBS's built-in
 * obs-websocket server (OBS 28 or newer).
 *
 * {@link connect} reaches OBS with the password the user set; {@link Obs.configure} puts a MoQ relay
 * or an RTMP(S) server in a dedicated profile tuned for low latency, {@link Obs.start} goes live, and
 * {@link Obs.stream} and {@link Obs.stats} follow it. {@link Obs.addSource} plays a broadcast in a scene.
 *
 * @module
 */

export { ConnectError, type ConnectReason, RequestError } from "./error.ts";
export {
	type Configured,
	type ConnectProps,
	connect,
	type Destination,
	type Ignored,
	Obs,
	PORT,
	type Publish,
	type Source,
	type Stats,
	type StreamState,
	type Version,
} from "./obs.ts";
