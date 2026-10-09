/**
 * The refused session: dial the relay with a token its public rules refuse, and report how the
 * WebTransport session ended.
 *
 * The relay accepts the CONNECT, then refuses the token and closes the session with a code and a
 * reason in a CLOSE_WEBTRANSPORT_SESSION capsule. Chromium is the peer that loses that capsule when
 * the server ends its HTTP/3 control stream early, so this reads `WebTransport.closed` directly
 * instead of trusting a client library's translation of it.
 *
 * @module
 */
import { Connection } from "@moq/net";
import type { CloseState } from "./contract";

/** Dial `relay` with a refused token and resolve with how the session closed. */
export async function refused(relay: string): Promise<CloseState> {
	const url = new URL(relay);

	// The relay's self-signed certificate, pinned the way @moq/net does for an http:// URL.
	const fingerprintUrl = new URL("/certificate.sha256", url);
	const fingerprint = (await (await fetch(fingerprintUrl)).text()).trim();
	const hash = Uint8Array.from(fingerprint.match(/../g) ?? [], (byte) => Number.parseInt(byte, 16));

	url.protocol = "https:";
	// Public rules verify nothing, so the relay refuses any token rather than ignoring it.
	url.searchParams.set("jwt", "refused");

	const transport = new WebTransport(url, { serverCertificateHashes: [{ algorithm: "sha-256", value: hash }] });
	const closed: Promise<CloseState> = transport.closed.then(
		(info) => ({ closeCode: info.closeCode ?? 0, reason: info.reason ?? "" }),
		(err: unknown) => ({ error: String(err) }),
	);

	// @moq/net only drives the handshake far enough for the relay to decide; the close it reports
	// is not what this checks. It takes a supplied transport as already connected.
	const admitted = transport.ready.then(() => Connection.connect({ url, transport })).then(
		(session): CloseState => {
			session.abort();
			return { error: "the relay admitted a refused token" };
		},
		(err: unknown) => {
			console.log(`connect failed: ${String(err)}`);
			return closed;
		},
	);

	return Promise.race([closed, admitted]);
}
