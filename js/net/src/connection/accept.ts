import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import { Stream } from "../stream.ts";
import type { Established } from "./established.ts";
import { exchangeSetup } from "./handshake.ts";

/** Options for {@link accept}. */
export interface AcceptProps {
	// Version to select during SETUP negotiation (for non-ALPN paths).
	version?: number;
}

/**
 * Server-side handshake: accepts a transport and performs the server half of the SETUP exchange.
 *
 * @param transport - The WebTransport session to accept
 * @param url - The URL of the connection
 * @param props - Optional configuration
 * @returns A promise that resolves to a Connection instance
 */
export async function accept(transport: WebTransport, url: URL, props?: AcceptProps): Promise<Established> {
	// @ts-expect-error - TODO: add protocol to WebTransport
	const protocol: string | undefined = transport.protocol;

	if (protocol === Ietf.ALPN.DRAFT_19) {
		return acceptAlpn(transport, url, Ietf.Version.DRAFT_19);
	} else if (protocol === Ietf.ALPN.DRAFT_18) {
		return acceptAlpn(transport, url, Ietf.Version.DRAFT_18);
	} else if (protocol === Ietf.ALPN.DRAFT_17) {
		return acceptAlpn(transport, url, Ietf.Version.DRAFT_17);
	} else if (protocol === Ietf.ALPN.DRAFT_16) {
		return acceptSetup(transport, url, Ietf.Version.DRAFT_16);
	} else if (protocol === Ietf.ALPN.DRAFT_15) {
		return acceptSetup(transport, url, Ietf.Version.DRAFT_15);
	} else if (protocol === Lite.ALPN_05) {
		return new Lite.Connection(url, transport, Lite.Version.DRAFT_05, undefined);
	} else if (protocol === Lite.ALPN_04) {
		return new Lite.Connection(url, transport, Lite.Version.DRAFT_04, undefined);
	} else if (protocol === Lite.ALPN_03) {
		return new Lite.Connection(url, transport, Lite.Version.DRAFT_03, undefined);
	} else if (protocol === Lite.ALPN || protocol === "" || protocol === undefined) {
		return acceptNegotiated(transport, url, props);
	} else {
		throw new Error(`unsupported WebTransport protocol: ${protocol}`);
	}
}

/**
 * Draft-17+ accept: ALPN already pinned the version. SETUP is exchanged over
 * a pair of uni streams using stream type 0x2F00.
 */
async function acceptAlpn(transport: WebTransport, url: URL, version: Ietf.IetfVersion): Promise<Established> {
	const controlStream = await exchangeSetup(transport, version, "moq-lite-js");

	return new Ietf.Connection({
		url,
		quic: transport,
		control: controlStream,
		// v17+ uses NativeSession which manages its own request IDs; maxRequestId is unused.
		maxRequestId: 0n,
		version,
	});
}

/**
 * Legacy accept (draft-15/16): ALPN pinned the version, but the SETUP message
 * is still exchanged over a bidi stream wrapped in the moq-lite compat envelope.
 */
async function acceptSetup(transport: WebTransport, url: URL, version: Ietf.IetfVersion): Promise<Established> {
	// Accept bidi, read ClientSetup, write ServerSetup
	const stream = await Stream.accept(transport);
	if (!stream) throw new Error("no incoming bidi stream for SETUP");

	const clientCompat = await stream.reader.u53();
	if (clientCompat !== Lite.StreamId.ClientCompat) {
		throw new Error(`unexpected client message type: 0x${clientCompat.toString(16)}`);
	}

	await Ietf.ClientSetup.decode(stream.reader, version);

	await stream.writer.u53(Lite.StreamId.ServerCompat);

	const encoder = new TextEncoder();
	const params = new Ietf.SetupOptions();
	params.setVarint(Ietf.SetupOption.MaxRequestId, 42069n);
	params.setBytes(Ietf.SetupOption.Implementation, encoder.encode("moq-lite-js"));

	const server = new Ietf.ServerSetup({ version, parameters: params });
	await server.encode(stream.writer, version);

	const maxRequestId = 42069n;

	return new Ietf.Connection({
		url,
		quic: transport,
		control: stream,
		maxRequestId,
		version,
	});
}

async function acceptNegotiated(transport: WebTransport, url: URL, props?: AcceptProps): Promise<Established> {
	const setupVersion = Ietf.Version.DRAFT_14;

	const stream = await Stream.accept(transport);
	if (!stream) throw new Error("no incoming bidi stream for SETUP");

	const clientCompat = await stream.reader.u53();
	if (clientCompat !== Lite.StreamId.ClientCompat) {
		throw new Error(`unexpected client message type: 0x${clientCompat.toString(16)}`);
	}

	const client = await Ietf.ClientSetup.decode(stream.reader, setupVersion);

	// Pick the requested version, or first matching version from client's list
	const allVersions = [...Object.values(Lite.Version), ...Object.values(Ietf.Version)] as number[];
	let selectedVersion: number;
	if (props?.version !== undefined) {
		selectedVersion = props.version;
	} else {
		const match = client.versions.find((v) => allVersions.includes(v));
		if (match === undefined) {
			throw new Error(
				`no common version found; client offered: ${client.versions.map((v) => v.toString(16)).join(", ")}`,
			);
		}
		selectedVersion = match;
	}

	await stream.writer.u53(Lite.StreamId.ServerCompat);

	const encoder = new TextEncoder();
	const params = new Ietf.SetupOptions();
	params.setVarint(Ietf.SetupOption.MaxRequestId, 42069n);
	params.setBytes(Ietf.SetupOption.Implementation, encoder.encode("moq-lite-js"));

	const server = new Ietf.ServerSetup({ version: selectedVersion, parameters: params });
	await server.encode(stream.writer, setupVersion);

	if (Object.values(Lite.Version).includes(selectedVersion as Lite.Version)) {
		return new Lite.Connection(url, transport, selectedVersion as Lite.Version, stream);
	} else if (Object.values(Ietf.Version).includes(selectedVersion as Ietf.Version)) {
		const maxRequestId = client.parameters.getVarint(Ietf.SetupOption.MaxRequestId) ?? 0n;
		return new Ietf.Connection({
			url,
			quic: transport,
			control: stream,
			maxRequestId,
			version: selectedVersion as Ietf.IetfVersion,
		});
	} else {
		throw new Error(`unsupported version: ${selectedVersion.toString(16)}`);
	}
}
