import { Milli, Nano, sleep } from "@moq/clock";
import Session, { type Version as QmuxVersion } from "@moq/qmux";
import { error, fromClose, ProtocolViolation, sessionCause } from "../error.ts";
import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import type { Consumer as OriginConsumer, Producer as OriginProducer } from "../origin.ts";
import { Stream } from "../stream.ts";
import * as Hex from "../util/hex.ts";
import { redact } from "../util/log.ts";
import { isWebTransportSupported } from "./browser.ts";
import type { Established } from "./established.ts";
import * as Extensions from "./extensions.ts";
import { DISCOVERY_REMOVED, forwardAnnounced } from "./forward.ts";
import { exchangeSetup } from "./handshake.ts";

// Default head start for WebTransport before attempting the WebSocket fallback.
const DEFAULT_WEBSOCKET_DELAY_MS = Milli(500);

// RESET_STREAM and CONNECTION_CLOSE are separate transport notifications. Give the latter
// time to surface its authoritative code without letting a reset-only peer stall SETUP forever.
const SETUP_CLOSE_GRACE = Nano.fromMilli(Milli(100));

/** Tuning for the WebSocket fallback used when WebTransport is unavailable or loses the connect race. */
export interface WebSocketProps {
	/** Enable the WebSocket fallback. Defaults to `true`. */
	enabled?: boolean;

	/** Use a different URL than WebTransport. By default, `https` maps to `wss` and `http` to `ws`. */
	url?: URL;

	/**
	 * The delay in milliseconds before attempting the WebSocket fallback (default: 500).
	 * If WebSocket won the previous race for a given URL, this is 0.
	 */
	delay?: Milli;

	/**
	 * The protocols to offer, most preferred first, as `webtransport.protocols` does for
	 * WebTransport. Defaults to every supported protocol except the work-in-progress
	 * `moq-lite-07-wip`; an unsupported one throws.
	 */
	protocols?: string[];
}

/**
 * A server certificate hash used to pin a self-signed server, one entry of
 * `serverCertificateHashes`. Unlike the DOM type, `value` also accepts a hex string (the
 * format moq servers report via their certificate fingerprints), decoded automatically.
 */
export interface CertificateHash {
	/** The hash algorithm. Defaults to `sha-256` (the only supported value). */
	algorithm?: "sha-256";
	/** The certificate hash: raw bytes or a hex string. */
	value: BufferSource | string;
}

/** WebTransport options extended with friendlier certificate pinning (hex hashes or a raw certificate). */
export interface WebTransportProps extends Omit<WebTransportOptions, "serverCertificateHashes"> {
	/**
	 * Pin the server to one or more certificate hashes. Each `value` may be raw
	 * bytes or a hex string; the algorithm defaults to `sha-256`.
	 */
	serverCertificateHashes?: CertificateHash[];

	/**
	 * Pin the server by supplying its certificate directly; the SHA-256 hash is
	 * computed for you. Accepts a PEM string or raw DER bytes. Use this when you
	 * have the certificate but not its precomputed fingerprint.
	 */
	serverCertificate?: string | BufferSource;
}

/** Options for {@link connect}. */
export interface ConnectProps {
	/** The relay URL. */
	url: URL;

	/** WebTransport options. */
	webtransport?: WebTransportProps;

	/** WebSocket (fallback) options. */
	websocket?: WebSocketProps;

	/**
	 * Use a pre-existing WebTransport session instead of connecting; skips the
	 * WebTransport/WebSocket race. The publisher acquires the session's datagram
	 * writer lock (`datagrams.writable.getWriter()`) for the session's lifetime,
	 * so the caller must not hold it. Aborting via {@link ConnectProps.signal}
	 * closes this session, so don't share one you intend to reuse afterwards.
	 */
	transport?: WebTransport;

	/**
	 * The origin whose broadcasts the session announces and serves to the peer. Omit to
	 * publish nothing.
	 *
	 * The origin is borrowed, not owned: closing the session leaves its broadcasts alone,
	 * and the same origin can back several sessions (or successive reconnects), each
	 * announcing the table for as long as it lasts.
	 */
	publish?: OriginConsumer;

	/**
	 * The origin the session feeds with the peer's announced broadcasts. Omit to discover
	 * nothing.
	 *
	 * What the peer announces under the prefixes the origin's readers watch
	 * (`announced(scope)` or `request(path, { announced: true })`) appears in its table for
	 * the session's lifetime, consumable by path; nothing is asked until something watches,
	 * and the entries retract when the session dies. Pass the producer behind
	 * {@link ConnectProps.publish} to route both directions through one origin: a locally
	 * published path is then served back to local consumers directly, and the session still
	 * never announces the peer's own broadcasts back to it.
	 */
	consume?: OriginProducer;

	/** The moq-transport extensions to offer, each on unless set to `false`. */
	extensions?: Extensions.Extensions;

	/**
	 * Aborts the connection attempt with the signal's reason. An already-aborted
	 * signal rejects before anything opens, and aborting after the connection is
	 * established has no effect. Use `AbortSignal.timeout(ms)` for a deadline.
	 */
	signal?: AbortSignal;
}

/** The per-session wiring shared by every negotiated protocol path. */
type SessionProps = {
	publish?: OriginConsumer;
	/** Whether this side dialed; only the dialing side aborts on a publication its grant does not cover. */
	client: boolean;
	/** The moq-transport extensions this side offers. */
	extensions: Extensions.Offered;
};

// Save if WebSocket won the last race, so we won't give QUIC a head start next time.
const websocketWon = new Set<string>();

/** The default connect signal: never aborts. */
const NEVER_ABORTED = new AbortController().signal;

/**
 * Establishes a connection to a MOQ server.
 *
 * @param url - The URL of the server to connect to
 * @param props - Connection options
 * @returns A promise that resolves to an established session
 */
export async function connect({ url, ...props }: ConnectProps): Promise<Established> {
	if ("discovery" in props) throw new TypeError(DISCOVERY_REMOVED);
	const signal = props.signal ?? NEVER_ABORTED;
	signal.throwIfAborted();

	// Resolves on abort so every in-flight transport tears itself down.
	const { promise: abort, resolve } = Promise.withResolvers<void>();
	const onAbort = () => resolve();
	signal.addEventListener("abort", onAbort, { once: true });

	const pending = connectInner(url, props, abort);
	try {
		// A `pending` rejection propagates unless the abort beat it to the finish line.
		const connection = await Promise.race([pending, abort.then(() => undefined)]);
		if (connection && !signal.aborted) {
			if (props.consume) forwardAnnounced(connection, props.consume);
			return connection;
		}

		// Close a connection that settles after the abort.
		pending.then((conn) => conn.abort()).catch(() => {});
		throw signal.reason;
	} finally {
		signal.removeEventListener("abort", onAbort);
	}
}

async function connectInner(url: URL, props: Omit<ConnectProps, "url">, abort: Promise<void>): Promise<Established> {
	const wiring: SessionProps = {
		publish: props.publish,
		client: true,
		extensions: Extensions.offered(props.extensions),
	};

	if (props.transport) {
		const transport = props.transport;
		void abort.then(() => transport.close());
		return connectTransport(url, transport, wiring);
	}

	// Resolved before racing, so a misconfigured protocol fails the call rather than one transport.
	const versions = props.websocket?.enabled !== false ? websocketVersions(props.websocket?.protocols) : undefined;

	// Stop transports after one connects or the caller aborts.
	const { promise: raced, resolve: done } = Promise.withResolvers<void>();
	const cancel = Promise.race([raced, abort]);

	const webtransport = isWebTransportSupported() ? connectWebTransport(url, cancel, props.webtransport) : undefined;

	// Give QUIC a head start to connect before trying WebSocket, unless WebSocket has won in the past.
	// NOTE that QUIC should be faster because it involves 1/2 fewer RTTs.
	const headstart =
		!webtransport || websocketWon.has(url.toString()) ? 0 : (props.websocket?.delay ?? DEFAULT_WEBSOCKET_DELAY_MS);
	const websocket = versions ? connectWebSocket(props.websocket?.url ?? url, headstart, cancel, versions) : undefined;

	if (!websocket && !webtransport) {
		throw new Error("no transport available; WebTransport not supported and WebSocket is disabled");
	}

	// Race the available transports, using `.any` to ignore if one participant has an error.
	// `webtransport`/`websocket` are `Promise | undefined`, so test existence explicitly: a
	// promise is always truthy, so bare truthiness here would be a misused-promise.
	const session = await Promise.any(
		webtransport !== undefined
			? websocket !== undefined
				? [websocket, webtransport]
				: [webtransport]
			: [websocket],
	);
	done();

	if (!session) throw new Error("no transport available");

	// Abort the setup handshake without leaking the selected transport.
	void abort.then(() => session.close());

	// Save if WebSocket won the last race, so we won't give QUIC a head start next time.
	if (session instanceof Session) {
		console.debug(redact(url), "connected via WebSocket");
		websocketWon.add(url.toString());
	} else {
		console.debug(redact(url), "connected via WebTransport");
	}

	// The remaining setup is identical whether the transport was raced or supplied.
	return await connectTransport(url, session as WebTransport, wiring);
}

async function connectTransport(url: URL, session: WebTransport, wiring: SessionProps): Promise<Established> {
	const closed = session.closed.then(fromClose, error);
	const terminal = closed.then((cause) => {
		throw cause ?? new Error("session closed during SETUP");
	});

	try {
		return await Promise.race([terminal, negotiate(url, session, wiring)]);
	} catch (cause) {
		// We refused the peer's SETUP and already closed with the code that says why.
		if (cause instanceof ProtocolViolation) throw cause;
		// A session failure already guarantees a terminal close; its authoritative code
		// must not be overwritten by the reset-only fallback while the browser delivers it.
		cause = await sessionCause(session, cause);
		// A session shutdown can reject its SETUP stream before `closed` publishes the
		// peer's close code. Wait briefly without closing locally, since a local clean close
		// could overwrite that code. A reset-only peer is cleaned up after the grace period.
		const pending = Symbol("session close pending");
		const final = await Promise.race([closed, sleep(SETUP_CLOSE_GRACE).then(() => pending)]);
		if (final !== pending) {
			if (final) throw final;
			throw cause;
		}

		session.close();
		throw cause;
	}
}

// Negotiate the MoQ protocol over an established transport. The caller races this against
// the session closing so a close code is not lost behind a failed or stalled SETUP stream.
async function negotiate(url: URL, session: WebTransport, wiring: SessionProps): Promise<Established> {
	// The DOM lib has no `protocol` property yet. It is "" when none was negotiated, and
	// undefined in a browser that predates subprotocols (Firefox before 155).
	const protocol: string | undefined = (session as { protocol?: string }).protocol || undefined;
	console.debug(redact(url), "negotiated ALPN:", protocol ?? "(none)");

	// Choose setup encoding based on negotiated WebTransport protocol (if any).
	let setupVersion: Ietf.Version;
	const modernVersion =
		protocol === Ietf.ALPN.DRAFT_22
			? Ietf.Version.DRAFT_22
			: protocol === Ietf.ALPN.DRAFT_21
				? Ietf.Version.DRAFT_21
				: protocol === Ietf.ALPN.DRAFT_20
					? Ietf.Version.DRAFT_20
					: protocol === Ietf.ALPN.DRAFT_19
						? Ietf.Version.DRAFT_19
						: protocol === Ietf.ALPN.DRAFT_18
							? Ietf.Version.DRAFT_18
							: protocol === Ietf.ALPN.DRAFT_17
								? Ietf.Version.DRAFT_17
								: undefined;
	if (modernVersion !== undefined) {
		return await handshakeAlpn(url, session, modernVersion, wiring);
	} else if (protocol === Ietf.ALPN.DRAFT_16) {
		setupVersion = Ietf.Version.DRAFT_16;
	} else if (protocol === Ietf.ALPN.DRAFT_15) {
		setupVersion = Ietf.Version.DRAFT_15;
	} else if (protocol === Lite.ALPN_07_WIP) {
		return new Lite.Connection({ url, quic: session, version: Lite.Version.DRAFT_07, ...wiring });
	} else if (protocol === Lite.ALPN_06) {
		return new Lite.Connection({ url, quic: session, version: Lite.Version.DRAFT_06, ...wiring });
	} else if (protocol === Lite.ALPN_05) {
		return new Lite.Connection({ url, quic: session, version: Lite.Version.DRAFT_05, ...wiring });
	} else if (protocol === Lite.ALPN_04) {
		return new Lite.Connection({ url, quic: session, version: Lite.Version.DRAFT_04, ...wiring });
	} else if (protocol === Lite.ALPN_03) {
		return new Lite.Connection({ url, quic: session, version: Lite.Version.DRAFT_03, ...wiring });
	} else if (protocol === Lite.ALPN || protocol === "" || protocol === undefined) {
		setupVersion = Ietf.Version.DRAFT_14;
	} else {
		throw new Error(`unsupported WebTransport protocol: ${protocol}`);
	}

	const stream = await Stream.open(session, { version: setupVersion });
	await stream.writer.u53(Lite.StreamId.ClientCompat);

	const encoder = new TextEncoder();

	const params = new Ietf.SetupOptions();
	params.setVarint(Ietf.SetupOption.MaxRequestId, Ietf.initialMaxRequestId(false));
	params.setBytes(Ietf.SetupOption.Implementation, encoder.encode("moq-lite-js"));
	Extensions.intoSetup(params, wiring.extensions, setupVersion);
	Ietf.hiddenIntoSetup(params);
	Ietf.activeCountIntoSetup(params, setupVersion);

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
		return new Lite.Connection({
			url,
			quic: session,
			version: server.version as Lite.Version,
			session: stream,
			...wiring,
		});
	} else if (Object.values(Ietf.Version).includes(server.version as Ietf.Version)) {
		const maxRequestId = server.parameters.getVarint(Ietf.SetupOption.MaxRequestId) ?? 0n;
		return new Ietf.Connection({
			...wiring,
			client: true,
			url,
			quic: session,
			control: stream,
			maxRequestId,
			version: server.version as Ietf.IetfVersion,
			solicit: Ietf.solicitFromSetup(server.parameters),
			hidden: Ietf.hiddenFromSetup(server.parameters),
			activeCount: Ietf.activeCountFromSetup(server.parameters, server.version as Ietf.IetfVersion),
		});
	} else {
		throw new Error(`unsupported server version: ${server.version.toString()}`);
	}
}

/**
 * Draft-17+ client handshake. ALPN already pinned the version; SETUP is
 * exchanged over a pair of uni streams using stream type 0x2F00.
 */
async function handshakeAlpn(
	url: URL,
	session: WebTransport,
	version: Ietf.IetfVersion,
	wiring: SessionProps,
): Promise<Established> {
	const { control, early, solicit, hidden, activeCount, cluster, auth, epoch } = await exchangeSetup(
		session,
		version,
		"moq-lite-js",
		wiring,
	);

	return new Ietf.Connection({
		...wiring,
		client: true,
		url,
		quic: session,
		control,
		early,
		solicit,
		hidden,
		activeCount,
		cluster,
		auth,
		epoch,
		// v17+ uses NativeSession which manages its own request IDs; maxRequestId is unused.
		maxRequestId: 0n,
		version,
	});
}

// One entry of the DOM `serverCertificateHashes`, derived without naming the lib type.
type WebTransportHash = NonNullable<WebTransportOptions["serverCertificateHashes"]>[number];

// Strip PEM armor and base64-decode to the raw DER bytes.
function pemToDer(pem: string): Uint8Array<ArrayBuffer> {
	const match = pem.match(/-----BEGIN CERTIFICATE-----([\s\S]+?)-----END CERTIFICATE-----/);
	if (!match) {
		throw new Error("invalid PEM certificate: missing -----BEGIN/END CERTIFICATE----- armor");
	}

	const binary = atob(match[1].replace(/\s+/g, ""));
	const der = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i++) {
		der[i] = binary.charCodeAt(i);
	}
	return der;
}

/**
 * Compute the SHA-256 hash of a certificate, the value `serverCertificateHashes`
 * pins. Accepts a PEM string or raw DER bytes. Matches the hex fingerprints a moq
 * server reports, so `Hex.fromBytes(await certificateHash(pem))` round-trips.
 */
export async function certificateHash(cert: string | BufferSource): Promise<Uint8Array<ArrayBuffer>> {
	const der = typeof cert === "string" ? pemToDer(cert) : cert;
	const digest = await crypto.subtle.digest("SHA-256", der);
	return new Uint8Array(digest);
}

// Normalize our friendlier pinning options into the DOM `serverCertificateHashes`.
async function resolveCertificateHashes(options?: WebTransportProps): Promise<WebTransportHash[] | undefined> {
	const hashes: WebTransportHash[] = [];

	for (const hash of options?.serverCertificateHashes ?? []) {
		const value = typeof hash.value === "string" ? Hex.toBytes(hash.value) : hash.value;
		hashes.push({ algorithm: hash.algorithm ?? "sha-256", value });
	}

	if (options?.serverCertificate !== undefined) {
		hashes.push({ algorithm: "sha-256", value: await certificateHash(options.serverCertificate) });
	}

	return hashes.length > 0 ? hashes : undefined;
}

async function connectWebTransport(
	url: URL,
	cancel: Promise<void>,
	options?: WebTransportProps,
): Promise<WebTransport | undefined> {
	let finalUrl = url;

	// Our custom pinning fields are normalized separately; the rest are DOM options.
	const { serverCertificate: _cert, serverCertificateHashes: _hashes, ...webtransport } = options ?? {};

	const finalOptions: WebTransportOptions = {
		allowPooling: false,
		congestionControl: "low-latency",
		protocols: [
			// Lite.ALPN_07_WIP is intentionally omitted: lite-07 is work-in-progress and
			// not advertised by default (negotiate still accepts it if a server selects it).
			Lite.ALPN_06,
			Lite.ALPN_05,
			Lite.ALPN_04,
			Lite.ALPN_03,
			Lite.ALPN,
			Ietf.ALPN.DRAFT_22,
			Ietf.ALPN.DRAFT_21,
			Ietf.ALPN.DRAFT_20,
			Ietf.ALPN.DRAFT_19,
			Ietf.ALPN.DRAFT_18,
			Ietf.ALPN.DRAFT_17,
			Ietf.ALPN.DRAFT_16,
			Ietf.ALPN.DRAFT_15,
		],
		...webtransport,
	};

	// Accumulate caller-provided pins first, then append anything we fetch below,
	// so a fetched fingerprint never clobbers hashes passed in via options.
	const hashes = (await resolveCertificateHashes(options)) ?? [];

	// Only perform certificate fetch and URL rewrite when polyfill is not needed
	// This is needed because WebTransport is a butt to work with in local development.
	if (url.protocol === "http:") {
		const fingerprintUrl = new URL(url);
		fingerprintUrl.pathname = "/certificate.sha256";
		// The certificate endpoint is unauthenticated, so don't hand it the token.
		fingerprintUrl.search = "";

		// Dev-only path: http:// can't be a real WebTransport origin, so we fetch the
		// self-signed cert's hash over plain HTTP and pin it. Production uses https://
		// and never reaches here. Keep this at debug so it doesn't read as a problem.
		console.debug(redact(fingerprintUrl), "performing an insecure fingerprint fetch; use https:// in production");

		// Fetch the fingerprint from the server.
		// TODO cancel the request if the effect is cancelled.
		const fingerprint = await Promise.race([fetch(fingerprintUrl), cancel]);
		if (!fingerprint) return undefined;

		const fingerprintText = await Promise.race([fingerprint.text(), cancel]);
		if (fingerprintText === undefined) return undefined;

		hashes.push({ algorithm: "sha-256", value: Hex.toBytes(fingerprintText) });

		finalUrl = new URL(url);
		finalUrl.protocol = "https:";
	}

	if (hashes.length > 0) {
		finalOptions.serverCertificateHashes = hashes;
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

// moq-transport-18 and newer are pinned to qmux-01. Every other ALPN we
// support is currently negotiated as `qmux-00.{alpn}` on the wire, but we
// don't want to lock that in: set the value to `null` so the polyfill
// advertises every QMux draft it knows about and the server picks one.
// Insertion order is the negotiation preference on the wire.
const WEBSOCKET_VERSIONS = {
	[Lite.ALPN_06]: null,
	[Lite.ALPN_05]: null,
	[Lite.ALPN_04]: null,
	[Lite.ALPN_03]: null,
	[Lite.ALPN]: null,
	[Ietf.ALPN.DRAFT_22]: "qmux-01",
	[Ietf.ALPN.DRAFT_21]: "qmux-01",
	[Ietf.ALPN.DRAFT_20]: "qmux-01",
	[Ietf.ALPN.DRAFT_19]: "qmux-01",
	[Ietf.ALPN.DRAFT_18]: "qmux-01",
	[Ietf.ALPN.DRAFT_17]: null,
	[Ietf.ALPN.DRAFT_16]: null,
	[Ietf.ALPN.DRAFT_15]: null,
} as const satisfies Record<string, QmuxVersion | QmuxVersion[] | null>;

// The QMux versions to offer per ALPN: `protocols` in their order, or the defaults. Lite-07 is
// work-in-progress, so it is offered only when asked for.
function websocketVersions(protocols?: string[]): Record<string, QmuxVersion | QmuxVersion[] | null> {
	if (!protocols) return WEBSOCKET_VERSIONS;
	const supported: Record<string, QmuxVersion | QmuxVersion[] | null> = {
		[Lite.ALPN_07_WIP]: null,
		...WEBSOCKET_VERSIONS,
	};
	const versions: Record<string, QmuxVersion | QmuxVersion[] | null> = {};
	for (const protocol of protocols) {
		if (!Object.hasOwn(supported, protocol)) throw new Error(`unsupported WebSocket protocol: ${protocol}`);
		versions[protocol] = supported[protocol];
	}
	return versions;
}

// TODO accept arguments to control the port/path used.
async function connectWebSocket(
	url: URL,
	delay: number,
	cancel: Promise<void>,
	versions: Record<string, QmuxVersion | QmuxVersion[] | null>,
): Promise<Session | undefined> {
	const timer = sleep(Nano.fromMilli(Milli(delay)));

	const active = await Promise.race([cancel, timer.then(() => true)]);
	if (!active) return undefined;

	// The default (`requireProtocol: false`) also advertises bare `qmux-01`,
	// `qmux-00`, and `webtransport` so we still interop with relays that only
	// know a wire-format version (today's moq-relay only accepts bare
	// `webtransport`).
	const quic = new Session(url, {
		protocols: Object.keys(versions),
		versions,
	});

	// Wait for the WebSocket to connect, or for the cancel promise to resolve.
	// `ready` rejects on a refused/failed connection, so a throw here is the caller's
	// cue to retry; a lost cancel race instead resolves and we close the loser.
	const loaded = await Promise.race([quic.ready.then(() => true), cancel]);
	if (!loaded) {
		quic.close();
		return undefined;
	}

	return quic;
}
