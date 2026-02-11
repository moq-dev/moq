import WebTransportWs from "@moq/web-transport-ws";
import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import { Stream } from "../stream.ts";
import * as Hex from "../util/hex.ts";
import type { Established } from "./established.ts";

export interface WebSocketOptions {
	// If true (default), enable the WebSocket fallback.
	enabled?: boolean;

	// Optional: Use a different URL than WebTransport.
	// By default, `https` => `wss` and `http` => `ws`.
	url?: URL;

	// The delay in milliseconds before attempting the WebSocket fallback. (default: 200)
	// If WebSocket won the previous race for a given URL, this will be 0.
	delay?: DOMHighResTimeStamp;
}

export interface ConnectProps {
	// WebTransport options.
	webtransport?: WebTransportOptions;

	// WebSocket (fallback) options.
	websocket?: WebSocketOptions;
}

// Save if WebSocket won the last race, so we won't give QUIC a head start next time.
const websocketWon = new Set<string>();

/**
 * Establishes a connection to a MOQ server.
 *
 * @param url - The URL of the server to connect to
 * @returns A promise that resolves to a Connection instance
 */
export async function connect(url: URL, props?: ConnectProps): Promise<Established> {
	// Create a cancel promise to kill whichever is still connecting.
	let done: (() => void) | undefined;
	const cancel = new Promise<void>((resolve) => {
		done = resolve;
	});

	const webtransport = globalThis.WebTransport ? connectWebTransport(url, cancel, props?.webtransport) : undefined;

	// Give QUIC a 200ms head start to connect before trying WebSocket, unless WebSocket has won in the past.
	// NOTE that QUIC should be faster because it involves 1/2 fewer RTTs.
	const headstart = !webtransport || websocketWon.has(url.toString()) ? 0 : (props?.websocket?.delay ?? 200);
	const websocket =
		props?.websocket?.enabled !== false
			? connectWebSocket(props?.websocket?.url ?? url, headstart, cancel)
			: undefined;

	if (!websocket && !webtransport) {
		throw new Error("no transport available; WebTransport not supported and WebSocket is disabled");
	}

	// Race them, using `.any` to ignore if one participant has a error.
	const session = await Promise.any(
		webtransport ? (websocket ? [websocket, webtransport] : [webtransport]) : [websocket],
	);
	if (done) done();

	if (!session) throw new Error("no transport available");

	// Save if WebSocket won the last race, so we won't give QUIC a head start next time.
	if (session instanceof WebTransportWs) {
		console.warn(url.toString(), "using WebSocket fallback; the user experience may be degraded");
		websocketWon.add(url.toString());
	}

	// moq-rs currently requires the ROLE extension to be set.
	const stream = await Stream.open(session);

	// @ts-expect-error - TODO: add protocol to WebTransport
	const protocol = session instanceof WebTransport ? session.protocol : undefined;

	// Choose setup encoding based on negotiated WebTransport protocol (if any).
	let setupVersion: Ietf.Version;
	if (protocol === Ietf.ALPN.DRAFT_15) {
		setupVersion = Ietf.Version.DRAFT_15;
	} else if (protocol === undefined) {
		setupVersion = Ietf.Version.DRAFT_14;
	} else {
		throw new Error(`unsupported WebTransport protocol: ${protocol}`);
	}

	// We're encoding 0x20 so it's backwards compatible with moq-transport-10+
	await stream.writer.u53(Lite.StreamId.ClientCompat);

	const encoder = new TextEncoder();

	const params = new Ietf.Parameters();
	params.setVarint(Ietf.Parameter.MaxRequestId, 42069n); // Allow a ton of request IDs.
	params.setBytes(Ietf.Parameter.Implementation, encoder.encode("moq-lite-js")); // Put the implementation name in the parameters.

	const client = new Ietf.ClientSetup(
		// NOTE: draft 15 onwards does not use CLIENT_SETUP to negotiate the version.
		// We still echo it just to make sure we're not accidentally trying to negotiate the version.
		setupVersion === Ietf.Version.DRAFT_15
			? [Ietf.Version.DRAFT_15]
			: [Lite.Version.DRAFT_02, Lite.Version.DRAFT_01, Ietf.Version.DRAFT_14],
		params,
	);
	console.debug(url.toString(), "sending client setup", client);
	await client.encode(stream.writer, setupVersion);

	// And we expect 0x21 as the response.
	const serverCompat = await stream.reader.u53();
	if (serverCompat !== Lite.StreamId.ServerCompat) {
		throw new Error(`unsupported server message type: ${serverCompat.toString()}`);
	}

	// Decode ServerSetup in Draft14 format (version + params)
	const server = await Ietf.ServerSetup.decode(stream.reader, setupVersion);
	console.debug(url.toString(), "received server setup", server);

	if (Object.values(Lite.Version).includes(server.version as Lite.Version)) {
		console.debug(url.toString(), "moq-lite session established");
		return new Lite.Connection(url, session, stream, server.version as Lite.Version);
	} else if (Object.values(Ietf.Version).includes(server.version as Ietf.Version)) {
		const maxRequestId = server.parameters.getVarint(Ietf.Parameter.MaxRequestId) ?? 0n;
		console.debug(url.toString(), "moq-ietf session established, version:", server.version.toString(16));
		return new Ietf.Connection(url, session, stream, maxRequestId, server.version as Ietf.IetfVersion);
	} else {
		throw new Error(`unsupported server version: ${server.version.toString()}`);
	}
}

async function connectWebTransport(
	url: URL,
	cancel: Promise<void>,
	options?: WebTransportOptions,
): Promise<WebTransport | undefined> {
	let finalUrl = url;

	const finalOptions: WebTransportOptions = {
		allowPooling: false,
		congestionControl: "low-latency",
		// @ts-expect-error - TODO: add protocols to WebTransportOptions
		protocols: [Ietf.ALPN.DRAFT_15],
		...options,
	};

	// Only perform certificate fetch and URL rewrite when polyfill is not needed
	// This is needed because WebTransport is a butt to work with in local development.
	if (url.protocol === "http:") {
		const fingerprintUrl = new URL(url);
		fingerprintUrl.pathname = "/certificate.sha256";
		fingerprintUrl.search = "";
		console.warn(fingerprintUrl.toString(), "performing an insecure fingerprint fetch; use https:// in production");

		// Fetch the fingerprint from the server.
		// TODO cancel the request if the effect is cancelled.
		const fingerprint = await Promise.race([fetch(fingerprintUrl), cancel]);
		if (!fingerprint) return undefined;

		const fingerprintText = await Promise.race([fingerprint.text(), cancel]);
		if (fingerprintText === undefined) return undefined;

		finalOptions.serverCertificateHashes = (finalOptions.serverCertificateHashes || []).concat([
			{
				algorithm: "sha-256",
				value: Hex.toBytes(fingerprintText),
			},
		]);

		finalUrl = new URL(url);
		finalUrl.protocol = "https:";
	}

	const quic = new WebTransport(finalUrl, finalOptions);

	// Wait for the WebTransport to connect, or for the cancel promise to resolve.
	// Close the connection if we lost the race.
	const loaded = await Promise.race([quic.ready.then(() => true), cancel]);
	if (!loaded) {
		quic.close();
		return undefined;
	}

	return quic;
}

// TODO accept arguments to control the port/path used.
async function connectWebSocket(url: URL, delay: number, cancel: Promise<void>): Promise<WebTransportWs | undefined> {
	const timer = new Promise<void>((resolve) => setTimeout(resolve, delay));

	const active = await Promise.race([cancel, timer.then(() => true)]);
	if (!active) return undefined;

	if (delay) {
		console.debug(url.toString(), `no WebTransport after ${delay}ms, attempting WebSocket fallback`);
	}

	const quic = new WebTransportWs(url);

	// Wait for the WebSocket to connect, or for the cancel promise to resolve.
	// Close the connection if we lost the race.
	const loaded = await Promise.race([quic.ready.then(() => true), cancel]);
	if (!loaded) {
		quic.close();
		return undefined;
	}

	return quic;
}
