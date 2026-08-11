import * as Ietf from "../ietf/index.ts";
import { Reader, Stream, Writer } from "../stream.ts";

/**
 * Draft-17+ SETUP exchange. Each side opens a uni stream, writes its Setup
 * message, and reads the peer's Setup off an incoming uni stream. The two
 * halves run in parallel and the protocol is symmetric, so both `connect`
 * (client) and `accept` (server) use this same function.
 */
export async function exchangeSetup(
	transport: WebTransport,
	version: Ietf.IetfVersion,
	implementation: string,
): Promise<Stream> {
	const encoder = new TextEncoder();
	const params = new Ietf.SetupOptions();
	params.setBytes(Ietf.SetupOption.Implementation, encoder.encode(implementation));
	const setupMsg = new Ietf.Setup({ parameters: params });

	const [writer, reader] = await Promise.all([
		sendSetup(transport, version, setupMsg),
		receiveSetup(transport, version),
	]);

	return new Stream({ writer, reader });
}

async function sendSetup(transport: WebTransport, version: Ietf.IetfVersion, setupMsg: Ietf.Setup): Promise<Writer> {
	// Via Writer.open for its deadline: a peer can advertise a stream limit of zero and
	// never raise it, which would otherwise hang the handshake rather than failing it.
	const writer = await Writer.open(transport, { version });
	await writer.u53(Ietf.Setup.id); // 0x2F00 stream type
	await setupMsg.encode(writer, version);
	return writer;
}

async function receiveSetup(transport: WebTransport, version: Ietf.IetfVersion): Promise<Reader> {
	const uniReader = transport.incomingUnidirectionalStreams.getReader() as ReadableStreamDefaultReader<
		ReadableStream<Uint8Array>
	>;
	const next = await uniReader.read();
	uniReader.releaseLock();
	if (next.done) throw new Error("no incoming uni stream for SETUP");

	const reader = new Reader(next.value, undefined, version);

	const streamType = await reader.u53();
	if (streamType !== Ietf.Setup.id) {
		throw new Error(`unexpected stream type on setup uni: 0x${streamType.toString(16)}`);
	}
	await Ietf.Setup.decode(reader, version);

	return reader;
}
