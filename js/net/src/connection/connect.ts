import { Elapsed, Milli, Nano, sleep, timeout } from "@moq/clock";
import Session, { type Version as QmuxVersion } from "@moq/qmux";
import { race } from "@moq/signals";
import type * as Auth from "../auth.ts";
import { vacant } from "../auth_session.ts";
import { ConnectTimeout, error, fromClose, ProtocolViolation, sessionCause } from "../error.ts";
import * as Ietf from "../ietf/index.ts";
import { IMPLEMENTATION } from "../implementation.ts";
import * as Lite from "../lite/index.ts";
import type { Consumer as OriginConsumer, Producer as OriginProducer } from "../origin.ts";
import { Stream } from "../stream.ts";
import * as Hex from "../util/hex.ts";
import { redact } from "../util/log.ts";
import { after } from "../util/timer.ts";
import { isWebTransportSupported } from "./browser.ts";
import type { Established } from "./established.ts";
import * as Extensions from "./extensions.ts";
import { DISCOVERY_REMOVED, forwardAnnounced } from "./forward.ts";
import { DEFAULT_TIMEOUT, exchangeSetup, handsOff } from "./handshake.ts";
import { credential, fallback, hold, type Presentation } from "./tokens.ts";

// Default head start for WebTransport before attempting the WebSocket fallback.
const DEFAULT_WEBSOCKET_DELAY_MS = Milli(500);

// How long the first WebTransport dial may stay unopened before a second one dials alongside
// it. The same moment as the WebSocket's default head start, so a fresh WebTransport flow
// races the fallback rather than losing to it, which would leave the session on WebSocket for
// good. A healthy open is about two round trips, so this spares round trips up to about
// 250 ms, and it is inside the 2 s ceiling RFC 8305 recommends for staggered attempts.
const WEBTRANSPORT_HEDGE = DEFAULT_WEBSOCKET_DELAY_MS;

// How long a WebSocket dial may stay unopened before it is closed and dialed again. Generous,
// since script cannot tell a silent connect from a slow one; Firefox and socket.io give a
// WebSocket 20 s to open.
const WEBSOCKET_REDIAL = Milli(10_000);

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

/**
 * WebTransport options extended with friendlier certificate pinning (hex hashes or a raw certificate).
 * The subprotocols come from {@link ConnectProps.versions}, shared with the WebSocket fallback.
 */
export interface WebTransportProps extends Omit<WebTransportOptions, "serverCertificateHashes" | "protocols"> {
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

// Each version name, most preferred first, with the ALPN that negotiates it. `setup` is the
// version code offered in SETUP, used when the transport negotiated `moql` or no ALPN at all;
// moq-transport has no version list in SETUP.
const VERSIONS: Record<string, { alpn: string; setup?: Lite.Version }> = {
	"moq-lite-07-wip": { alpn: Lite.ALPN_07_WIP, setup: Lite.Version.DRAFT_07 },
	"moq-lite-06": { alpn: Lite.ALPN_06, setup: Lite.Version.DRAFT_06 },
	"moq-lite-05": { alpn: Lite.ALPN_05, setup: Lite.Version.DRAFT_05 },
	"moq-lite-04": { alpn: Lite.ALPN_04, setup: Lite.Version.DRAFT_04 },
	"moq-lite-03": { alpn: Lite.ALPN_03, setup: Lite.Version.DRAFT_03 },
	"moq-lite-02": { alpn: Lite.ALPN, setup: Lite.Version.DRAFT_02 },
	"moq-lite-01": { alpn: Lite.ALPN, setup: Lite.Version.DRAFT_01 },
	"moq-transport-22": { alpn: Ietf.ALPN.DRAFT_22 },
	"moq-transport-18": { alpn: Ietf.ALPN.DRAFT_18 },
};

// Experimental lite-07 is offered only when named.
const DEFAULT_VERSIONS = Object.keys(VERSIONS).filter((name) => name !== "moq-lite-07-wip");

// moq-transport is pinned to qmux-01. Every other ALPN is currently
// negotiated as `qmux-00.{alpn}`, but `null` lets the polyfill advertise every QMux draft
// it knows and the server pick one.
const QMUX: Record<string, QmuxVersion | QmuxVersion[] | null> = {
	[Ietf.ALPN.DRAFT_22]: "qmux-01",
	[Ietf.ALPN.DRAFT_18]: "qmux-01",
};

/** What one version list offers: the ALPNs in preference order, and the versions offered in SETUP. */
interface Offer {
	alpns: string[];
	setup: Lite.Version[];
	/** Whether the caller named the versions, so the peer must pick one of them. */
	explicit: boolean;
	/** Whether every offered version carries the connection credential in SETUP. */
	setupToken: boolean;
}

// moq-lite gained the SETUP token in lite-07; every moq-transport draft has one.
function hasSetupToken(name: string): boolean {
	return !name.startsWith("moq-lite-") || name === "moq-lite-07-wip";
}

function offer(versions?: readonly string[]): Offer {
	const names = versions ?? DEFAULT_VERSIONS;
	const alpns = new Set<string>();
	const setup: Lite.Version[] = [];
	for (const name of names) {
		const version = VERSIONS[name];
		if (!version) throw new TypeError(`unsupported version: ${name}`);
		alpns.add(version.alpn);
		if (version.setup !== undefined) setup.push(version.setup);
	}
	return { alpns: [...alpns], setup, explicit: versions !== undefined, setupToken: names.every(hasSetupToken) };
}

/** Copy a version list that can be offered, throwing a `TypeError` if it is empty or has an unsupported or duplicate name. */
export function checkVersions(versions: readonly string[]): string[] {
	if (versions.length === 0) throw new TypeError("versions must not be empty");
	const seen = new Set<string>();
	for (const name of versions) {
		if (!Object.hasOwn(VERSIONS, name)) throw new TypeError(`unsupported version: ${name}`);
		if (seen.has(name)) throw new TypeError(`duplicate version: ${name}`);
		seen.add(name);
	}
	return [...versions];
}

/** Validate and snapshot an ordered version list before acquiring a connection. @internal */
export function normalizeVersions(props?: {
	versions?: readonly string[];
	webtransport?: WebTransportProps;
	websocket?: WebSocketProps;
}): string[] | undefined {
	if (props?.webtransport && "protocols" in props.webtransport)
		throw new TypeError("webtransport.protocols was removed; set versions instead");
	if (props?.websocket && "protocols" in props.websocket)
		throw new TypeError("websocket.protocols was removed; set versions instead");
	return props?.versions === undefined ? undefined : checkVersions(props.versions);
}

/** Options for {@link Connection.connect}. */
export interface ConnectProps {
	/** The relay URL. */
	url: URL;

	/**
	 * Protocol versions to offer on every transport, most preferred first, spelled like
	 * `moq-lite-06`. Defaults to every supported version except the work-in-progress
	 * `moq-lite-07-wip`; list every fallback you still want. With a peer that negotiates no
	 * subprotocol (such as WebTransport in Firefox before 155), the version is negotiated in
	 * SETUP instead, which moq-transport cannot do.
	 */
	versions?: readonly string[];

	/** WebTransport options. */
	webtransport?: WebTransportProps;

	/** WebSocket (fallback) options. */
	websocket?: WebSocketProps;

	/**
	 * Use a pre-existing WebTransport session instead of connecting; skips the
	 * WebTransport/WebSocket race. The publisher acquires the session's datagram
	 * writer lock (`datagrams.writable.getWriter()`) for the session's lifetime,
	 * so the caller must not hold it. Aborting via {@link ConnectProps.signal}, or
	 * {@link ConnectProps.timeout} passing, closes this session, so don't share one
	 * you intend to reuse afterwards.
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
	 * (`announced(scope)`, `request(path)`, or `follow(path)`) appears in its table for
	 * the session's lifetime, consumable by path; nothing is asked until something watches,
	 * and the entries retract when the session dies. Pass the producer behind
	 * {@link ConnectProps.publish} to route both directions through one origin: a locally
	 * published path is then served back to local consumers directly, and the session still
	 * never announces the peer's own broadcasts back to it.
	 */
	consume?: OriginProducer;

	/**
	 * The moq-transport extensions to offer, each on unless set to `false`.
	 *
	 * @internal Tests only; nothing outside the package consumes it.
	 */
	extensions?: Extensions.Extensions;

	/**
	 * Credentials to present, kept apart from the URL.
	 *
	 * The first is the connection credential: it rides SETUP, and the URL's `jwt` query too
	 * while an offered version can't carry it in SETUP (moq-lite before 07). Each other token
	 * rides its own AUTH stream, widening the session's grant. A URL that already has a `jwt`
	 * keeps it as the connection credential, and then every token rides AUTH.
	 */
	tokens?: readonly string[];

	/**
	 * Build the session on `auth`, so it presents every token the handle holds and a route
	 * naming one of its `Credential`s keeps it across a reconnect. Omit to give the session a handle
	 * of its own, which ends with it.
	 *
	 * The handle backs one active session at a time, plus the ones draining after a GOAWAY or a
	 * `close()`: connecting another while one is still active rejects with {@link Auth.Duplicate}.
	 */
	auth?: Auth.Handle;

	/**
	 * The implementation this side advertises in SETUP. Defaults to {@link IMPLEMENTATION};
	 * a wrapper prepends its own token, like `my-app/1.0 moq-net-js/0.4.2`. Empty sends nothing.
	 */
	implementation?: string;

	/**
	 * Maximum time for one connect, covering the dial and the MoQ handshake, which has no
	 * deadline of its own (default: 30000ms). `0` waits forever. Past it, the connect rejects
	 * with {@link ConnectTimeout} and closes whatever it had opened.
	 */
	timeout?: Milli;

	/**
	 * Aborts the connection attempt with the signal's reason. An already-aborted
	 * signal rejects before anything opens, and aborting after the connection is
	 * established has no effect. Set {@link ConnectProps.timeout} for a deadline.
	 */
	signal?: AbortSignal;
}

/** The per-session wiring shared by every negotiated protocol path. */
type SessionProps = {
	publish?: OriginConsumer;
	/** The auth handle the session is built on, or undefined for one of its own. */
	handle?: Auth.Handle;
	/** Whether this side dialed; only the dialing side aborts on a publication its grant does not cover. */
	client: boolean;
	/** The moq-transport extensions this side offers. */
	extensions: Extensions.Offered;
	/** The connection credential SETUP presents, on versions that carry one. */
	token?: Uint8Array;
	/** The implementation this side advertises in SETUP; empty sends nothing. */
	implementation: string;
};

// Save if WebSocket won the last race, so we won't give QUIC a head start next time.
const websocketWon = new Set<string>();

/** The default connect signal: never aborts. */
const NEVER_ABORTED = new AbortController().signal;

/**
 * Establishes a connection to a MOQ server.
 *
 * @returns A promise that resolves to an established session
 */
export async function connect({ url, ...props }: ConnectProps): Promise<Established> {
	if ("discovery" in props) throw new TypeError(DISCOVERY_REMOVED);
	props.versions = normalizeVersions(props);
	if (props.transport && props.versions) throw new TypeError("versions cannot reconfigure a supplied transport");
	const signal = props.signal ?? NEVER_ABORTED;
	signal.throwIfAborted();
	// Fail before dialing; the session checks again when it attaches.
	if (props.auth) vacant(props.auth);
	const limit = props.timeout ?? DEFAULT_TIMEOUT;
	// Checked before anything is armed, so an invalid value throws here, in the caller's unit.
	const deadline = Nano.fromMilli(Milli(limit));

	// Resolves on abort or timeout so every in-flight transport tears itself down.
	const { promise: abort, resolve } = Promise.withResolvers<void>();
	const onAbort = () => resolve();
	signal.addEventListener("abort", onAbort, { once: true });

	let expired: ConnectTimeout | undefined;
	const disarm =
		limit > 0
			? after(deadline, () => {
					expired = new ConnectTimeout(limit);
					resolve();
				})
			: () => {};

	const tokens = props.tokens ?? [];
	const pending = connectInner(url, tokens, props, abort);
	try {
		// A `pending` rejection propagates unless the abort or timeout beat it to the finish line.
		const connected = await Promise.race([pending, abort.then(() => undefined)]);
		if (connected && !signal.aborted && !expired) {
			const [connection, dial] = connected;
			if (props.consume) forwardAnnounced(connection, props.consume);
			// Before the caller sees the session, so its first requests wait on these answers.
			hold(connection, dial, tokens);
			return connection;
		}

		// Close a connection that settles after the abort or timeout.
		pending.then(([conn]) => conn.abort()).catch(() => {});
		// The caller's abort is teardown and wins over the timeout.
		throw signal.aborted ? signal.reason : expired;
	} finally {
		signal.removeEventListener("abort", onAbort);
		disarm();
	}
}

/**
 * The URL a browser transport dials. A raw QUIC `moqt:` or `moql:` URL is reached over
 * WebTransport with its scheme swapped for https (draft-ietf-moq-transport-18 §3.1.3); the
 * CONNECT carries the scheme, so a server refuses the raw one. Rebuilt rather than assigned,
 * since the URL setter refuses a change from a non-special scheme to a special one.
 */
function dialable(url: URL): URL {
	if (url.protocol !== "moqt:" && url.protocol !== "moql:") return url;
	return new URL(`https:${url.href.slice(url.protocol.length)}`);
}

/** Dial `raw`, resolving with the session and the credential the winning transport presented. */
async function connectInner(
	raw: URL,
	tokens: readonly string[],
	props: Omit<ConnectProps, "url">,
	abort: Promise<void>,
): Promise<[Established, Presentation]> {
	const offered = offer(props.versions);
	const dial = credential(raw, tokens, offered.setupToken);
	const url = dialable(dial.url);
	const fallbackDial = props.websocket?.url && fallback(raw, props.websocket.url, tokens, offered.setupToken);
	const wiring = (presented: Presentation): SessionProps => ({
		publish: props.publish,
		handle: props.auth,
		client: true,
		extensions: Extensions.offered(props.extensions),
		token: presented.setup === undefined ? undefined : new TextEncoder().encode(presented.setup),
		implementation: props.implementation ?? IMPLEMENTATION,
	});

	if (props.transport) {
		const transport = props.transport;
		void abort.then(() => transport.close());
		return [await connectTransport(url, transport, wiring(dial), offered.setup), dial];
	}

	// Stop transports after one connects or the caller aborts.
	const { promise: raced, resolve: done } = Promise.withResolvers<void>();
	const cancel = Promise.race([raced, abort]);

	const webtransport = isWebTransportSupported()
		? hedgeWebTransport(url, cancel, offered.alpns, props.webtransport)
		: undefined;

	// Give QUIC a head start to connect before trying WebSocket, unless WebSocket has won in the past.
	// NOTE that QUIC should be faster because it involves 1/2 fewer RTTs.
	const headstart =
		!webtransport || websocketWon.has(url.toString()) ? 0 : (props.websocket?.delay ?? DEFAULT_WEBSOCKET_DELAY_MS);
	const websocket =
		props.websocket?.enabled !== false
			? connectWebSocket(fallbackDial?.url ?? url, headstart, cancel, offered.alpns)
			: undefined;

	if (!websocket && !webtransport) {
		throw new Error("no transport available; WebTransport not supported and WebSocket is disabled");
	}

	// Race the available transports, using `.any` to ignore if one participant has an error.
	// `webtransport`/`websocket` are `Promise | undefined`, so test existence explicitly: a
	// promise is always truthy, so bare truthiness here would be a misused-promise.
	const contenders: Promise<WebTransport | Session | undefined>[] = [];
	if (websocket !== undefined) contenders.push(websocket);
	if (webtransport !== undefined) contenders.push(webtransport);
	const session = await Promise.any(contenders).catch((err: unknown) => {
		// Promise.any rejects with an empty message; name each transport's failure instead.
		if (!(err instanceof AggregateError)) throw err;
		const reasons = err.errors.map((e: unknown) => (e instanceof Error ? e.message : String(e)));
		throw new Error(`no transport connected: ${reasons.join("; ")}`, { cause: err });
	});
	done();
	// A contender that opened in the same turn as the winner has already returned its
	// transport, so cancelling cannot close it; the rest close themselves on cancel.
	closeLosers(contenders, session);

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
	const protocol = (session as { protocol?: string }).protocol || undefined;
	if (offered.explicit && protocol && !offered.alpns.includes(protocol)) {
		session.close();
		throw new Error(`server selected an unoffered protocol: ${protocol}`);
	}
	if (offered.explicit && !protocol && offered.setup.length === 0) {
		session.close();
		throw new Error("server negotiated no protocol, and no offered version negotiates in SETUP");
	}
	const won = session instanceof Session && fallbackDial ? fallbackDial : dial;
	return [await connectTransport(url, session as WebTransport, wiring(won), offered.setup), won];
}

async function connectTransport(
	url: URL,
	session: WebTransport,
	wiring: SessionProps,
	setup: Lite.Version[],
): Promise<Established> {
	const closed = session.closed.then(fromClose, error);
	const terminal = closed.then((cause) => {
		throw cause ?? new Error("session closed during SETUP");
	});

	try {
		return await Promise.race([terminal, negotiate(url, session, wiring, setup)]);
	} catch (failure) {
		// We refused the peer's SETUP and already closed with the code that says why.
		if (failure instanceof ProtocolViolation) throw failure;
		// A session failure already guarantees a terminal close; its authoritative code
		// must not be overwritten by the reset-only fallback while the browser delivers it.
		const cause = await sessionCause(session, failure);
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
// `setup` lists the versions a `moql` or ALPN-less session offers in SETUP.
// A moq-lite version newer than 02 selected there continues as its ALPN would.
async function negotiate(
	url: URL,
	session: WebTransport,
	wiring: SessionProps,
	setup: Lite.Version[],
): Promise<Established> {
	// The DOM lib has no `protocol` property yet. It is "" when none was negotiated, and
	// undefined in a browser that predates subprotocols (Firefox before 155).
	const protocol: string | undefined = (session as { protocol?: string }).protocol || undefined;
	console.debug(redact(url), "negotiated ALPN:", protocol ?? "(none)");

	const ietf =
		protocol === Ietf.ALPN.DRAFT_22
			? Ietf.Version.DRAFT_22
			: protocol === Ietf.ALPN.DRAFT_18
				? Ietf.Version.DRAFT_18
				: undefined;
	if (ietf !== undefined) {
		return await handshakeAlpn(url, session, ietf, wiring);
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
	} else if (protocol !== Lite.ALPN && protocol !== undefined) {
		throw new Error(`unsupported WebTransport protocol: ${protocol}`);
	}

	// `moql`, or no subprotocol: negotiate moq-lite in SETUP, which keeps lite-02's framing
	// whichever version it offers.
	if (setup.length === 0) throw new Error("no version negotiates in SETUP");
	const stream = await Stream.open(session, { version: Lite.Version.DRAFT_02 });
	await new Lite.ClientSetup(setup).encode(stream.writer);
	const server = await Lite.ServerSetup.decode(stream.reader);
	const version = setup.find((v) => v === server.version);
	if (version === undefined) throw new Error(`unsupported server version: 0x${server.version.toString(16)}`);
	if (handsOff(version)) {
		// The server ends the stream after its SETUP; nothing else may follow.
		if (!(await stream.reader.done())) throw new Error("unexpected data after SERVER_SETUP");
		stream.close();
		return new Lite.Connection({ url, quic: session, version, ...wiring });
	}
	return new Lite.Connection({ url, quic: session, version, session: stream, ...wiring });
}

/**
 * Moq-transport client handshake. ALPN already pinned the version; SETUP is
 * exchanged over a pair of uni streams using stream type 0x2F00.
 */
async function handshakeAlpn(
	url: URL,
	session: WebTransport,
	version: Ietf.IetfVersion,
	wiring: SessionProps,
): Promise<Established> {
	// Spread whole, so every negotiated flag reaches the connection.
	// The IETF session code loads on demand, since most sessions negotiate lite; the ALPN already
	// committed to IETF, so fetch it alongside SETUP.
	const [setup, { Connection }] = await Promise.all([
		exchangeSetup(session, version, wiring.implementation, wiring),
		import("../ietf/connection.ts"),
	]);

	return new Connection({
		...wiring,
		client: true,
		url,
		quic: session,
		...setup,
		version,
	});
}

// One entry of the DOM `serverCertificateHashes`, derived without naming the lib type.
type WebTransportHash = NonNullable<WebTransportOptions["serverCertificateHashes"]>[number];

// Strip PEM armor and base64-decode to the raw DER bytes.
function pemToDer(pem: string): Uint8Array<ArrayBuffer> {
	// indexOf rather than a lazy regex, which goes quadratic on repeated BEGIN lines without an END.
	const begin = "-----BEGIN CERTIFICATE-----";
	const start = pem.indexOf(begin);
	const end = start < 0 ? -1 : pem.indexOf("-----END CERTIFICATE-----", start + begin.length);
	const body = end < 0 ? "" : pem.slice(start + begin.length, end).replace(/\s+/g, "");
	if (!body) {
		throw new Error("invalid PEM certificate: missing -----BEGIN/END CERTIFICATE----- armor");
	}

	const binary = atob(body);
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

// Close every transport the race opened other than `winner`.
function closeLosers(contenders: Promise<WebTransport | Session | undefined>[], winner: unknown): void {
	for (const contender of contenders) {
		void contender.then(
			(transport) => {
				if (transport && transport !== winner) transport.close();
			},
			() => {},
		);
	}
}

/**
 * Dial WebTransport, and dial it again alongside if the first has not opened within
 * {@link WEBTRANSPORT_HEDGE}. The first to open wins and the other is closed.
 *
 * A load balancer that still places some new flows on a target that just died sends every
 * packet of such a flow there, and the browser waits seconds before failing the dial. A
 * second dial is a new flow that the balancer places afresh. Neither dial is cut on a timer of
 * its own, only by the attempt's {@link ConnectProps.timeout}: the browser's handshake timer
 * tells a silent dial from a slow one, which script cannot, and every dial the browser fails
 * or abandons counts against its WebTransport throttle. A first dial that fails outright is
 * not hedged; that is left to the caller's retry.
 */
async function hedgeWebTransport(
	url: URL,
	cancel: Promise<void>,
	alpns: string[],
	options?: WebTransportProps,
): Promise<WebTransport | undefined> {
	const first = connectWebTransport(url, cancel, alpns, options);

	// The first dial opening, failing, or being cancelled settles this before the delay does.
	const hedge = await timeout(Nano.fromMilli(WEBTRANSPORT_HEDGE), first).then(
		() => false,
		(err: unknown) => err instanceof Elapsed,
	);
	if (!hedge) return first;
	console.debug(redact(url), `WebTransport not open after ${WEBTRANSPORT_HEDGE}ms; dialing a second`);

	const dials = [first, connectWebTransport(url, cancel, alpns, options)];
	try {
		const winner = await Promise.any(dials);
		closeLosers(dials, winner);
		return winner;
	} catch (err) {
		// Both failed. Report the first dial's reason, as when there is no hedge, so `no transport
		// connected` keeps one reason per transport; a browser words every handshake failure alike.
		throw (err as AggregateError).errors[0];
	}
}

async function connectWebTransport(
	url: URL,
	cancel: Promise<void>,
	alpns: string[],
	options?: WebTransportProps,
): Promise<WebTransport | undefined> {
	let finalUrl = url;

	// Our custom pinning fields are normalized separately; the rest are DOM options.
	const { serverCertificate: _cert, serverCertificateHashes: _hashes, ...webtransport } = options ?? {};

	const finalOptions: WebTransportOptions = {
		allowPooling: false,
		congestionControl: "low-latency",
		...webtransport,
		protocols: alpns,
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

// TODO accept arguments to control the port/path used.
/**
 * Dial the WebSocket fallback after `delay`, and if a dial has not opened within
 * {@link WEBSOCKET_REDIAL}, close it and dial again, doubling the wait each time.
 *
 * A browser lets one WebSocket to an address be connecting at a time (RFC 6455 section 4.1),
 * so a second dial alongside a silent one only queues behind it, and a TCP connect whose SYNs
 * go unanswered waits for the OS's connect timeout. Closing the silent dial lets the next one
 * start, on a new connection that a load balancer places afresh. The doubling lets a
 * slow but working path through, and {@link ConnectProps.timeout} bounds the whole attempt. A
 * dial that fails outright is not redialed.
 */
async function connectWebSocket(
	url: URL,
	delay: number,
	cancel: Promise<void>,
	alpns: string[],
): Promise<Session | undefined> {
	const timer = sleep(Nano.fromMilli(Milli(delay)));

	const active = await Promise.race([cancel, timer.then(() => true)]);
	if (!active) return undefined;

	for (let wait = WEBSOCKET_REDIAL; ; wait = Milli(wait * 2)) {
		// The default (`requireProtocol: false`) also advertises the bare `qmux-02`,
		// `qmux-01`, and `qmux-00`, so we still interop with relays that only know a
		// wire-format version.
		const quic = new Session(url, {
			protocols: alpns,
			versions: Object.fromEntries(alpns.map((alpn) => [alpn, QMUX[alpn] ?? null])),
		});

		// Wait for the WebSocket to connect, for the cancel promise to resolve, or for the
		// redial. `ready` rejects on a refused/failed connection, so a throw here is the
		// caller's cue to retry; a lost cancel race instead resolves and we close the loser.
		const disarm = new AbortController();
		try {
			// `race` rather than `Promise.race`, and `cancel` itself rather than something derived
			// from it: `cancel` outlives every dial, and `race` releases what it attached.
			const outcome = await race([
				quic.ready.then(() => "open" as const),
				cancel,
				sleep(Nano.fromMilli(wait), { signal: disarm.signal }).then(() => "redial" as const),
			]);
			if (outcome === "open") return quic;
			quic.close();
			if (outcome !== "redial") return undefined;
			console.debug(redact(url), `WebSocket not open after ${wait}ms; dialing again`);
		} finally {
			disarm.abort();
		}
	}
}
