import { Milli, Nano } from "@moq/clock";
import { race } from "@moq/signals";
import { ConnectTimeout, error, fromClose, ProtocolViolation } from "../error.ts";
import * as Ietf from "../ietf/index.ts";
import { IMPLEMENTATION } from "../implementation.ts";
import * as Lite from "../lite/index.ts";
import type { Consumer as OriginConsumer, Producer as OriginProducer } from "../origin.ts";
import { Stream } from "../stream.ts";
import { untilAborted } from "../util/abort.ts";
import { after } from "../util/timer.ts";
import type { Established } from "./established.ts";
import * as Extensions from "./extensions.ts";
import { DISCOVERY_REMOVED, forwardAnnounced } from "./forward.ts";
import { DEFAULT_TIMEOUT, exchangeSetup, handsOff } from "./handshake.ts";

/** Options for {@link Connection.accept}. */
export interface AcceptProps {
	/** The accepted transport. */
	transport: WebTransport;
	/** The request URL associated with the transport. */
	url: URL;

	/** Bound SETUP in milliseconds (default: 30000; zero waits until aborted). Throws ConnectTimeout. */
	timeout?: Milli;
	/** Abort the handshake and release the transport; ignored after acceptance completes. */
	signal?: AbortSignal;

	/** The moq-lite version to select in a `moql` SETUP, instead of the client's first. */
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
 * @returns A promise that resolves to an established session
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
	if (protocol === Ietf.ALPN.DRAFT_20) {
		return acceptAlpn(transport, url, Ietf.Version.DRAFT_20, wiring);
	}
	if (protocol === Ietf.ALPN.DRAFT_18) {
		return acceptAlpn(transport, url, Ietf.Version.DRAFT_18, wiring);
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
 * Moq-transport accept: ALPN already pinned the version. SETUP is exchanged over
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
		version,
	});
}

/** `moql`, or no subprotocol: negotiate moq-lite in SETUP. */
async function acceptNegotiated(
	transport: WebTransport,
	url: URL,
	wiring: SessionProps,
	version?: number,
): Promise<Established> {
	// The SETUP keeps lite-02's framing whichever version it offers.
	const stream = await Stream.accept(transport, Lite.Version.DRAFT_02);
	if (!stream) throw new Error("no incoming bidi stream for SETUP");

	const client = await Lite.ClientSetup.decode(stream.reader);
	const selected = version ?? client.versions.find(Lite.isNegotiated);
	if (selected === undefined || !Lite.isNegotiated(selected)) {
		throw new Error(
			`no common version found; client offered: ${client.versions.map((v) => v.toString(16)).join(", ")}`,
		);
	}

	await new Lite.ServerSetup(selected).encode(stream.writer);
	if (handsOff(selected)) {
		stream.close();
		return new Lite.Connection({ url, quic: transport, version: selected, ...wiring });
	}
	return new Lite.Connection({ url, quic: transport, version: selected, session: stream, ...wiring });
}
