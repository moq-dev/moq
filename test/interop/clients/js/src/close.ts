/**
 * The refused session: dial the relay with a token that is not a JWT, and report how the
 * WebTransport session ended.
 *
 * The relay accepts the CONNECT, then refuses the token and closes the session with a code and a
 * reason in a CLOSE_WEBTRANSPORT_SESSION capsule. Chromium is the peer that loses that capsule when
 * the server ends its HTTP/3 control stream early, so this reads `WebTransport.closed` directly
 * instead of trusting a client library's translation of it.
 *
 * @module
 */
import { Duration } from "@moq/clock";
import { Connection, Error as MoqError } from "@moq/net";
import type { CloseState } from "./contract";

/** Dial `relay` with a refused token and resolve with how the session closed. */
export async function refused(relay: string): Promise<CloseState> {
	const url = new URL(relay);

	// The relay's self-signed certificate, pinned the way @moq/net does for an http:// URL.
	const fingerprintUrl = new URL("/certificate.sha256", url);
	const fingerprint = (await (await fetch(fingerprintUrl)).text()).trim();
	const hash = Uint8Array.from(fingerprint.match(/../g) ?? [], (byte) => Number.parseInt(byte, 16));

	url.protocol = "https:";
	// Not a JWT, so the auth server refuses it as malformed rather than falling through to public.
	url.searchParams.set("jwt", "refused");

	const transport = new WebTransport(url, { serverCertificateHashes: [{ algorithm: "sha-256", value: hash }] });
	const closed: Promise<CloseState> = transport.closed.then(
		(info) => ({ closeCode: info.closeCode ?? 0, reason: info.reason ?? "" }),
		(err: unknown) => ({ error: String(err) }),
	);

	// @moq/net only drives the handshake far enough for the relay to decide; the close it reports
	// is not what this checks. A moq-lite session is established before the relay's verdict, so
	// the verdict is the close alone, and a relay that admits the token leaves this to time out.
	transport.ready
		.then(() => Connection.connect({ url, transport }))
		.catch((err: unknown) => console.log(`connect failed: ${String(err)}`));

	return closed;
}

/**
 * A WebSocket refusal reaches the reconnecting handle as its session error.
 *
 * The relay closes a session that ALPN already established, so the handle redials it like any
 * other close until its retry window runs out, then publishes the last close.
 */
export async function refusedWebSocket(relay: string): Promise<CloseState> {
	const url = new URL(relay);
	url.protocol = "ws:";
	url.searchParams.set("jwt", "refused");
	const connection = new Connection({
		url,
		share: false,
		delay: { initial: Duration.millis(200), max: Duration.millis(200), timeout: Duration.secs(2) },
	});
	// Every refused session counted as connected first; a second one proves the handle redialed.
	let connected = 0;
	const dispose = connection.status.subscribe((status) => {
		if (status === "connected") connected++;
	});
	try {
		let error = connection.error.peek();
		while (!error) error = await connection.error.changed();
		if (connected < 2) return { error: `gave up after ${connected} session(s) instead of redialing: ${error}` };
		if (!(error instanceof MoqError.Session)) return { error: String(error) };
		return { closeCode: error.code, reason: error.message };
	} finally {
		dispose();
		connection.close();
	}
}
