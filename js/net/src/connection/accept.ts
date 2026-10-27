import { Milli, Nano } from "@moq/clock";
import { race } from "@moq/signals";
import { ConnectTimeout, error, fromClose, ProtocolViolation } from "../error.ts";
import * as Ietf from "../ietf/index.ts";
import * as Message from "../ietf/message.ts";
import { IMPLEMENTATION } from "../implementation.ts";
import * as Lite from "../lite/index.ts";
import type { Consumer as OriginConsumer, Producer as OriginProducer } from "../origin.ts";
import { Stream } from "../stream.ts";
import { untilAborted } from "../util/abort.ts";
import { after } from "../util/timer.ts";
import type { Established } from "./established.ts";
import * as Extensions from "./extensions.ts";
import { DISCOVERY_REMOVED, forwardAnnounced } from "./forward.ts";
import { DEFAULT_TIMEOUT, exchangeSetup, handsOff, legacyPeerImplementation } from "./handshake.ts";

/** Options for {@link accept}. */
export interface AcceptProps {
	/** The accepted transport. */
	transport: WebTransport;
	/** The request URL associated with the transport. */
	url: URL;

	/** Bound SETUP in milliseconds (default: 30000; zero waits until aborted). Throws ConnectTimeout. */
	timeout?: Milli;
	/** Abort the handshake and release the transport; ignored after acceptance completes. */
	signal?: AbortSignal;

	/** Version to select during SETUP negotiation (for non-ALPN paths). */
	version?: number;

	/**
	 * The origin whose broadcasts the session announces and serves to the peer. Omit to
	 * publish nothing. Borrowed, not owned: closing the session leaves its broadcasts alone.
	 */
	publish?: OriginConsumer;

	/**
	 * The origin the session feeds with the peer's announced broadcasts. Omit to discover
	 * nothing. The entries retract when the session dies; see the `consume` connect option.
	 */
	consume?: OriginProducer;

	/**
	 * The moq-transport extensions to offer, each on unless set to `false`.
	 *
	 * @internal Tests only; nothing outside the package consumes it.
	 */
	extensions?: Extensions.Extensions;

	/** The implementation this side advertises in SETUP; see the `implementation` connect option. */
	implementation?: string;
}

/** The per-session wiring shared by every negotiated protocol path. */
type SessionProps = {
	publish?: OriginConsumer;
	/** Whether this side dialed; only the dialing side aborts on a publication its grant does not cover. */
	client: boolean;
	/** The moq-transport extensions this side offers. */
	extensions: Extensions.Offered;
	/** The implementation this side advertises in SETUP; empty sends nothing. */
	implementation: string;
};

/**
 * Server-side handshake: accepts a transport and performs the server half of the SETUP exchange.
 *
 * @param transport - The WebTransport session to accept
 * @param url - The URL of the connection
 * @param props - Optional configuration
 * @returns A promise that resolves to a Connection instance
 */
export async function accept({ transport, url, ...props }: AcceptProps): Promise<Established> {
	if ("discovery" in props) throw new TypeError(DISCOVERY_REMOVED);
	props.signal?.throwIfAborted();
	const limit = props.timeout ?? DEFAULT_TIMEOUT;
	const duration = Nano.fromMilli(Milli(limit));
	const deadline = new AbortController();
	const signal = props.signal ? AbortSignal.any([props.signal, deadline.signal]) : deadline.signal;
	const disarm = limit > 0 ? after(duration, () => deadline.abort(new ConnectTimeout(limit))) : () => {};
	const pending = acceptInner(transport, url, props);
	// As on connect, a dead transport must end a SETUP whose peer sends nothing more.
	const terminal = transport.closed.then(fromClose, error).then((cause) => {
		throw cause ?? new Error("session closed during SETUP");
	});
	try {
		const connection = await untilAborted(race([pending, terminal]), signal);
		if (signal.aborted) throw signal.reason;
		if (props.consume) forwardAnnounced(connection, props.consume);
		return connection;
	} catch (failure) {
		try {
			transport.close(
				failure instanceof ProtocolViolation ? { closeCode: failure.code, reason: failure.message } : undefined,
			);
		} catch {
			/* Already closed. */
		}
		// A handshake that resolves after cancellation must release its connection too.
		void pending.then(
			(connection) => connection.abort(),
			() => {},
		);
		throw props.signal?.aborted ? props.signal.reason : failure;
	} finally {
		disarm();
	}
}

async function acceptInner(
	transport: WebTransport,
	url: URL,
	props: Omit<AcceptProps, "transport" | "url">,
): Promise<Established> {
	// The DOM lib has no `protocol` property yet. It is "" when none was negotiated, and
	// undefined in a browser that predates subprotocols (Firefox before 155).
	const protocol = (transport as { protocol?: string }).protocol;

	const wiring: SessionProps = {
		publish: props.publish,
		client: false,
		extensions: Extensions.offered(props.extensions),
		implementation: props.implementation ?? IMPLEMENTATION,
	};

	if (protocol === Ietf.ALPN.DRAFT_22) {
		return acceptAlpn(transport, url, Ietf.Version.DRAFT_22, wiring);
	}
	if (protocol === Ietf.ALPN.DRAFT_18) {
		return acceptAlpn(transport, url, Ietf.Version.DRAFT_18, wiring);
	} else if (protocol === Ietf.ALPN.DRAFT_17) {
		return acceptAlpn(transport, url, Ietf.Version.DRAFT_17, wiring);
	} else if (protocol === Ietf.ALPN.DRAFT_16) {
		return acceptSetup(transport, url, Ietf.Version.DRAFT_16, wiring);
	} else if (protocol === Ietf.ALPN.DRAFT_15) {
		return acceptSetup(transport, url, Ietf.Version.DRAFT_15, wiring);
	} else if (protocol === Lite.ALPN_07_WIP) {
		return new Lite.Connection({ url, quic: transport, version: Lite.Version.DRAFT_07, ...wiring });
	} else if (protocol === Lite.ALPN_06) {
		return new Lite.Connection({ url, quic: transport, version: Lite.Version.DRAFT_06, ...wiring });
	} else if (protocol === Lite.ALPN_05) {
		return new Lite.Connection({ url, quic: transport, version: Lite.Version.DRAFT_05, ...wiring });
	} else if (protocol === Lite.ALPN_04) {
		return new Lite.Connection({ url, quic: transport, version: Lite.Version.DRAFT_04, ...wiring });
	} else if (protocol === Lite.ALPN_03) {
		return new Lite.Connection({ url, quic: transport, version: Lite.Version.DRAFT_03, ...wiring });
	} else if (protocol === Lite.ALPN || protocol === "" || protocol === undefined) {
		return acceptNegotiated(transport, url, wiring, props.version);
	} else {
		throw new Error(`unsupported WebTransport protocol: ${protocol}`);
	}
}

/**
 * Draft-17+ accept: ALPN already pinned the version. SETUP is exchanged over
 * a pair of uni streams using stream type 0x2F00.
 */
async function acceptAlpn(
	transport: WebTransport,
	url: URL,
	version: Ietf.IetfVersion,
	wiring: SessionProps,
): Promise<Established> {
	// Spread whole, so every negotiated flag reaches the connection.
	// The IETF session code loads on demand, since most sessions negotiate lite; the ALPN already
	// committed to IETF, so fetch it alongside SETUP.
	const [setup, { Connection }] = await Promise.all([
		exchangeSetup(transport, version, wiring.implementation, wiring),
		import("../ietf/connection.ts"),
	]);

	return new Connection({
		...wiring,
		client: false,
		url,
		quic: transport,
		...setup,
		// v17+ uses NativeSession which manages its own request IDs; maxRequestId is unused.
		maxRequestId: 0n,
		version,
	});
}

/**
 * Legacy accept (draft-15/16): ALPN pinned the version, but the SETUP message
 * is still exchanged over a bidi stream wrapped in the moq-lite compat envelope.
 */
async function acceptSetup(
	transport: WebTransport,
	url: URL,
	version: Ietf.IetfVersion,
	wiring: SessionProps,
): Promise<Established> {
	// Accept bidi, read ClientSetup, write ServerSetup
	const stream = await Stream.accept(transport, version);
	if (!stream) throw new Error("no incoming bidi stream for SETUP");

	const clientCompat = await stream.reader.u53();
	if (clientCompat !== Lite.StreamId.ClientCompat) {
		throw new Error(`unexpected client message type: 0x${clientCompat.toString(16)}`);
	}

	const client = await Ietf.ClientSetup.decode(stream.reader, version);
	const peerImplementation = legacyPeerImplementation(transport, client.parameters);

	const params = new Ietf.SetupOptions();
	params.setVarint(Ietf.SetupOption.MaxRequestId, Ietf.initialMaxRequestId(true));
	Ietf.implementationIntoSetup(params, wiring.implementation);
	Extensions.intoSetup(params, wiring.extensions, version);
	Ietf.hiddenIntoSetup(params);
	Ietf.maxAgeIntoSetup(params);
	Ietf.activeCountIntoSetup(params, version);

	const server = new Ietf.ServerSetup({ version, parameters: params });
	await Message.write(stream.writer, Lite.StreamId.ServerCompat, server, version);

	const maxRequestId = client.parameters.getVarint(Ietf.SetupOption.MaxRequestId) ?? 0n;

	// The IETF session code loads on demand, since most sessions negotiate lite.
	const { Connection } = await import("../ietf/connection.ts");
	return new Connection({
		...wiring,
		client: false,
		url,
		quic: transport,
		control: stream,
		maxRequestId,
		version,
		solicit: Ietf.solicitFromSetup(client.parameters),
		hidden: Ietf.hiddenFromSetup(client.parameters),
		maxAge: Ietf.maxAgeFromSetup(client.parameters),
		activeCount: Ietf.activeCountFromSetup(client.parameters, version),
		peerImplementation,
	});
}

async function acceptNegotiated(
	transport: WebTransport,
	url: URL,
	wiring: SessionProps,
	version?: number,
): Promise<Established> {
	const setupVersion = Ietf.Version.DRAFT_14;

	const stream = await Stream.accept(transport, setupVersion);
	if (!stream) throw new Error("no incoming bidi stream for SETUP");

	const clientCompat = await stream.reader.u53();
	if (clientCompat !== Lite.StreamId.ClientCompat) {
		throw new Error(`unexpected client message type: 0x${clientCompat.toString(16)}`);
	}

	const client = await Ietf.ClientSetup.decode(stream.reader, setupVersion);

	// Pick the requested version, or the first from the client's list that SETUP can negotiate:
	// every moq-lite version and moq-transport-14, since later drafts carry no version list.
	const negotiable = [...Object.values(Lite.Version), Ietf.Version.DRAFT_14] as number[];
	let selectedVersion: number;
	if (version !== undefined) {
		selectedVersion = version;
	} else {
		const match = client.versions.find((v) => negotiable.includes(v));
		if (match === undefined) {
			throw new Error(
				`no common version found; client offered: ${client.versions.map((v) => v.toString(16)).join(", ")}`,
			);
		}
		selectedVersion = match;
	}

	const params = new Ietf.SetupOptions();
	params.setVarint(Ietf.SetupOption.MaxRequestId, Ietf.initialMaxRequestId(true));
	Ietf.implementationIntoSetup(params, wiring.implementation);
	Extensions.intoSetup(params, wiring.extensions, setupVersion);
	Ietf.hiddenIntoSetup(params);
	Ietf.maxAgeIntoSetup(params);
	Ietf.activeCountIntoSetup(params, setupVersion);

	const server = new Ietf.ServerSetup({ version: selectedVersion, parameters: params });
	await Message.write(stream.writer, Lite.StreamId.ServerCompat, server, setupVersion);

	if (handsOff(selectedVersion)) {
		stream.close();
		return new Lite.Connection({ url, quic: transport, version: selectedVersion, ...wiring });
	}

	if (Object.values(Lite.Version).includes(selectedVersion as Lite.Version)) {
		return new Lite.Connection({
			url,
			quic: transport,
			version: selectedVersion as Lite.Version,
			session: stream,
			...wiring,
		});
	} else if (Object.values(Ietf.Version).includes(selectedVersion as Ietf.Version)) {
		const maxRequestId = client.parameters.getVarint(Ietf.SetupOption.MaxRequestId) ?? 0n;
		// The IETF session code loads on demand, since most sessions negotiate lite.
		const { Connection } = await import("../ietf/connection.ts");
		return new Connection({
			...wiring,
			client: false,
			url,
			quic: transport,
			control: stream,
			maxRequestId,
			version: selectedVersion as Ietf.IetfVersion,
			solicit: Ietf.solicitFromSetup(client.parameters),
			hidden: Ietf.hiddenFromSetup(client.parameters),
			maxAge: Ietf.maxAgeFromSetup(client.parameters),
			activeCount: Ietf.activeCountFromSetup(client.parameters, selectedVersion as Ietf.IetfVersion),
			peerImplementation: legacyPeerImplementation(transport, client.parameters),
		});
	} else {
		throw new Error(`unsupported version: ${selectedVersion.toString(16)}`);
	}
}
