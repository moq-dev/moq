import Session, { type Version as QmuxVersion } from "@moq/qmux";
import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import { Stream } from "../stream.ts";
import * as Hex from "../util/hex.ts";
import type { Established } from "./established.ts";
import { exchangeSetup } from "./handshake.ts";

// Default head start for WebTransport before attempting the WebSocket fallback.
const DEFAULT_WEBSOCKET_DELAY_MS = 500;

export interface WebSocketOptions {
	// If true (default), enable the WebSocket fallback.
	enabled?: boolean;

	// Optional: Use a different URL than WebTransport.
	// By default, `https` => `wss` and `http` => `ws`.
	url?: URL;

	// The delay in milliseconds before attempting the WebSocket fallback. (default: 500)
	// If WebSocket won the previous race for a given URL, this will be 0.
	delay?: DOMHighResTimeStamp;
}

export interface ConnectProps {
	// WebTransport options.
	webtransport?: WebTransportOptions;

	// WebSocket (fallback) options.
	websocket?: WebSocketOptions;

	// Use a pre-existing WebTransport session instead of connecting.
	// When provided, skips WebTransport/WebSocket race and uses this directly.
	transport?: WebTransport;
}

// Save if WebSocket won the last race, so we won't give QUIC a head start next time.
const websocketWon = new Set<string>();

// Firefox's WebTransport implementation drops server-initiated bidi streams,
// breaking publish (the relay opens a subscribe bidi back to us). Force WebSocket.
// TODO: remove once Firefox fixes incoming bidi delivery.
const isFirefox = typeof navigator !== "undefined" && navigator.userAgent.toLowerCase().includes("firefox");

/**
 * Establishes a connection to a MOQ server.
 *
 * @param url - The URL of the server to connect to
 * @returns A promise that resolves to a Connection instance
 */
export async function connect(url: URL, props?: ConnectProps): Promise<Established> {
	if (props?.transport) {
		return connectTransport(url, props.transport);
	}

	// Create a cancel promise to kill whichever is still connecting.
	const { promise: cancel, resolve: done } = Promise.withResolvers<void>();

	const webtransport =
		globalThis.WebTransport && !isFirefox ? connectWebTransport(url, cancel, props?.webtransport) : undefined;

	// Give QUIC a head start to connect before trying WebSocket, unless WebSocket has won in the past.
	// NOTE that QUIC should be faster because it involves 1/2 fewer RTTs.
	const headstart =
		!webtransport || websocketWon.has(url.toString()) ? 0 : (props?.websocket?.delay ?? DEFAULT_WEBSOCKET_DELAY_MS);
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
	done();

	if (!session) throw new Error("no transport available");

	// Save if WebSocket won the last race, so we won't give QUIC a head start next time.
	if (session instanceof Session) {
		console.warn(url.toString(), "connected via WebSocket");
		websocketWon.add(url.toString());
	} else {
		console.log(url.toString(), "connected via WebTransport");
	}

	// Get the negotiated protocol. qmux Session exposes it directly;
	// native WebTransport doesn't have a standard .protocol property yet.
	const protocol: string | undefined =
		session instanceof Session
			? session.protocol || undefined
			: // @ts-expect-error - TODO: add protocol to WebTransport
				session.protocol;
	console.debug(url.toString(), "negotiated ALPN:", protocol ?? "(none)");

	// Choose setup encoding based on negotiated WebTransport protocol (if any).
	let setupVersion: Ietf.Version;
	const modernVersion =
		protocol === Ietf.ALPN.DRAFT_18
			? Ietf.Version.DRAFT_18
			: protocol === Ietf.ALPN.DRAFT_17
				? Ietf.Version.DRAFT_17
				: undefined;
	if (modernVersion !== undefined) {
		return await handshakeAlpn(url, session as WebTransport, modernVersion);
	} else if (protocol === Ietf.ALPN.DRAFT_16) {
		setupVersion = Ietf.Version.DRAFT_16;
	} else if (protocol === Ietf.ALPN.DRAFT_15) {
		setupVersion = Ietf.Version.DRAFT_15;
	} else if (protocol === Lite.ALPN_04) {
		// moq-lite draft-04 doesn't use a session stream, so we return immediately.
		return new Lite.Connection(url, session, Lite.Version.DRAFT_04, undefined);
	} else if (protocol === Lite.ALPN_03) {
		// moq-lite draft-03 doesn't use a session stream, so we return immediately.
		return new Lite.Connection(url, session, Lite.Version.DRAFT_03, undefined);
	} else if (protocol === Lite.ALPN || protocol === "" || protocol === undefined) {
		// moq-lite ALPN (or no protocol) uses Draft14 encoding for SETUP,
		// then negotiates the actual version via the SETUP message.
		setupVersion = Ietf.Version.DRAFT_14;
	} else {
		throw new Error(`unsupported WebTransport protocol: ${protocol}`);
	}

	// We're encoding 0x20 so it's backwards compatible with moq-transport-10+
	const stream = await Stream.open(session);
	await stream.writer.u53(Lite.StreamId.ClientCompat);

	const encoder = new TextEncoder();

	const params = new Ietf.SetupOptions();
	params.setVarint(Ietf.SetupOption.MaxRequestId, 42069n); // Allow a ton of request IDs.
	params.setBytes(Ietf.SetupOption.Implementation, encoder.encode("moq-lite-js")); // Put the implementation name in the parameters.

	const client = new Ietf.ClientSetup({
		// NOTE: draft 15 onwards does not use CLIENT_SETUP to negotiate the version.
		// We still echo it just to make sure we're not accidentally trying to negotiate the version.
		versions:
			setupVersion === Ietf.Version.DRAFT_16
				? [Ietf.Version.DRAFT_16]
				: setupVersion === Ietf.Version.DRAFT_15
					? [Ietf.Version.DRAFT_15]
					: [Lite.Version.DRAFT_02, Lite.Version.DRAFT_01, Ietf.Version.DRAFT_14],
		parameters: params,
	});
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
		return new Lite.Connection(url, session, server.version as Lite.Version, stream);
	} else if (Object.values(Ietf.Version).includes(server.version as Ietf.Version)) {
		const maxRequestId = server.parameters.getVarint(Ietf.SetupOption.MaxRequestId) ?? 0n;
		return new Ietf.Connection({
			url,
			quic: session,
			control: stream,
			maxRequestId,
			version: server.version as Ietf.IetfVersion,
		});
	} else {
		throw new Error(`unsupported server version: ${server.version.toString()}`);
	}
}

async function connectTransport(url: URL, session: WebTransport): Promise<Established> {
	// @ts-expect-error - TODO: add protocol to WebTransport
	const protocol: string | undefined = session.protocol;

	// Choose setup encoding based on negotiated WebTransport protocol (if any).
	let setupVersion: Ietf.Version;
	const modernVersion =
		protocol === Ietf.ALPN.DRAFT_18
			? Ietf.Version.DRAFT_18
			: protocol === Ietf.ALPN.DRAFT_17
				? Ietf.Version.DRAFT_17
				: undefined;
	if (modernVersion !== undefined) {
		return await handshakeAlpn(url, session, modernVersion);
	} else if (protocol === Ietf.ALPN.DRAFT_16) {
		setupVersion = Ietf.Version.DRAFT_16;
	} else if (protocol === Ietf.ALPN.DRAFT_15) {
		setupVersion = Ietf.Version.DRAFT_15;
	} else if (protocol === Lite.ALPN_04) {
		return new Lite.Connection(url, session, Lite.Version.DRAFT_04, undefined);
	} else if (protocol === Lite.ALPN_03) {
		return new Lite.Connection(url, session, Lite.Version.DRAFT_03, undefined);
	} else if (protocol === Lite.ALPN || protocol === "" || protocol === undefined) {
		setupVersion = Ietf.Version.DRAFT_14;
	} else {
		throw new Error(`unsupported WebTransport protocol: ${protocol}`);
	}

	const stream = await Stream.open(session);
	await stream.writer.u53(Lite.StreamId.ClientCompat);

	const encoder = new TextEncoder();

	const params = new Ietf.SetupOptions();
	params.setVarint(Ietf.SetupOption.MaxRequestId, 42069n);
	params.setBytes(Ietf.SetupOption.Implementation, encoder.encode("moq-lite-js"));

	const client = new Ietf.ClientSetup({
		versions:
			setupVersion === Ietf.Version.DRAFT_16
				? [Ietf.Version.DRAFT_16]
				: setupVersion === Ietf.Version.DRAFT_15
					? [Ietf.Version.DRAFT_15]
					: [Lite.Version.DRAFT_02, Lite.Version.DRAFT_01, Ietf.Version.DRAFT_14],
		parameters: params,
	});
	await client.encode(stream.writer, setupVersion);

	const serverCompat = await stream.reader.u53();
	if (serverCompat !== Lite.StreamId.ServerCompat) {
		throw new Error(`unsupported server message type: ${serverCompat.toString()}`);
	}

	const server = await Ietf.ServerSetup.decode(stream.reader, setupVersion);

	if (Object.values(Lite.Version).includes(server.version as Lite.Version)) {
		return new Lite.Connection(url, session, server.version as Lite.Version, stream);
	} else if (Object.values(Ietf.Version).includes(server.version as Ietf.Version)) {
		const maxRequestId = server.parameters.getVarint(Ietf.SetupOption.MaxRequestId) ?? 0n;
		return new Ietf.Connection({
			url,
			quic: session,
			control: stream,
			maxRequestId,
			version: server.version as Ietf.IetfVersion,
		});
	} else {
		throw new Error(`unsupported server version: ${server.version.toString()}`);
	}
}

/**
 * Draft-17+ client handshake. ALPN already pinned the version; SETUP is
 * exchanged over a pair of uni streams using stream type 0x2F00.
 */
async function handshakeAlpn(url: URL, session: WebTransport, version: Ietf.IetfVersion): Promise<Established> {
	const controlStream = await exchangeSetup(session, version, "moq-lite-js");

	return new Ietf.Connection({
		url,
		quic: session,
		control: controlStream,
		// v17+ uses NativeSession which manages its own request IDs; maxRequestId is unused.
		maxRequestId: 0n,
		version,
	});
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
		protocols: [
			Lite.ALPN_04,
			Lite.ALPN_03,
			Lite.ALPN,
			Ietf.ALPN.DRAFT_18,
			Ietf.ALPN.DRAFT_17,
			Ietf.ALPN.DRAFT_16,
			Ietf.ALPN.DRAFT_15,
		],
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

	// Both .ready and .closed reject on failure; catch .closed to avoid an unhandled rejection.
	quic.closed.catch(() => {});

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
async function connectWebSocket(url: URL, delay: number, cancel: Promise<void>): Promise<Session | undefined> {
	const timer = new Promise<void>((resolve) => setTimeout(resolve, delay));

	const active = await Promise.race([cancel, timer.then(() => true)]);
	if (!active) return undefined;

	// Only moq-transport-18 is pinned to qmux-01 today. Every other ALPN we
	// support is currently negotiated as `qmux-00.{alpn}` on the wire, but we
	// don't want to lock that in: set the value to `null` so the polyfill
	// advertises every QMux draft it knows about and the server picks one.
	// Insertion order is the negotiation preference on the wire.
	const versions = {
		[Lite.ALPN_04]: null,
		[Lite.ALPN_03]: null,
		[Lite.ALPN]: null,
		[Ietf.ALPN.DRAFT_18]: "qmux-01",
		[Ietf.ALPN.DRAFT_17]: null,
		[Ietf.ALPN.DRAFT_16]: null,
		[Ietf.ALPN.DRAFT_15]: null,
	} as const satisfies Record<string, QmuxVersion | QmuxVersion[] | null>;

	// `withoutProtocol` also advertises bare `qmux-01`, `qmux-00`, and
	// `webtransport` so we still interop with relays that only know a
	// wire-format version (today's moq-relay only accepts bare `webtransport`).
	const quic = new Session(url, {
		protocols: Object.keys(versions),
		versions,
		withoutProtocol: true,
	});

	// Wait for the WebSocket to connect, or for the cancel promise to resolve.
	// Close the connection if we lost the race.
	const loaded = await Promise.race([quic.ready.then(() => true), cancel]);
	if (!loaded) {
		quic.close();
		return undefined;
	}

	return quic;
}
