import { StreamError } from "../error.ts";
import { type Hop, randomHop } from "../hop.ts";
import * as Ietf from "../ietf/index.ts";
import { Reader, Stream, UnexpectedEnd, Writer } from "../stream.ts";

/**
 * Draft-17+ SETUP exchange. Each side opens a uni stream, writes its Setup
 * message, and reads the peer's Setup off an incoming uni stream. The two
 * halves run in parallel and the protocol is symmetric, so both `connect`
 * (client) and `accept` (server) use this same function.
 *
 * Returns the control stream, the uni streams that arrived before it (see
 * {@link receiveSetup}), plus what the peer's SETUP declared: whether it requires
 * solicitation, which decides whether we announce namespaces unprompted (see the MoQ Solicit
 * extension), and its Hop ID (see the MoQ Cluster extension). We declare both ourselves on
 * every session: we send SUBSCRIBE_NAMESPACE for each prefix we want, so an unsolicited
 * advertisement can tell us nothing we won't have asked for, and a peer that knows our Hop
 * ID can withhold the advertisements that already flowed through us.
 */
export async function exchangeSetup(
	transport: WebTransport,
	version: Ietf.IetfVersion,
	implementation: string,
): Promise<{
	control: Stream;
	early: Reader[];
	solicit: boolean | undefined;
	hidden: boolean;
	cluster: Ietf.Cluster.Hops;
}> {
	const encoder = new TextEncoder();
	const params = new Ietf.SetupOptions();
	params.setBytes(Ietf.SetupOption.Implementation, encoder.encode(implementation));
	Ietf.solicitIntoSetup(params);
	Ietf.hiddenIntoSetup(params);

	// One id per session, like the moq-lite connection: nothing in this process forwards
	// between sessions, so there is nothing for a shared id to detect.
	const self = randomHop();
	Ietf.Cluster.intoSetup(params, self, version);

	const setupMsg = new Ietf.Setup({ parameters: params });

	const [writer, received] = await Promise.all([
		sendSetup(transport, version, setupMsg),
		receiveSetup(transport, version),
	]);

	return {
		control: new Stream({ writer, reader: received.reader }),
		early: received.early,
		solicit: received.solicit,
		hidden: received.hidden,
		cluster: { self, peer: received.cluster },
	};
}

async function sendSetup(transport: WebTransport, version: Ietf.IetfVersion, setupMsg: Ietf.Setup): Promise<Writer> {
	// Via Writer.open for its deadline: a peer can advertise a stream limit of zero and
	// never raise it, which would otherwise hang the handshake rather than failing it.
	const writer = await Writer.open(transport, { version });
	await writer.u53(Ietf.Setup.id); // 0x2F00 stream type
	await setupMsg.encode(writer, version);
	return writer;
}

/**
 * Read the peer's SETUP off its uni stream. Any other uni stream that beats it (padding,
 * or group data for a subscription the peer already holds) is held, type unread, for the
 * session to classify once it starts: the drafts say to buffer early data rather than
 * reject it. QUIC stream credit bounds how many can pile up.
 */
async function receiveSetup(
	transport: WebTransport,
	version: Ietf.IetfVersion,
): Promise<{
	reader: Reader;
	early: Reader[];
	solicit: boolean | undefined;
	hidden: boolean;
	cluster: Hop | undefined;
}> {
	const uniReader = transport.incomingUnidirectionalStreams.getReader() as ReadableStreamDefaultReader<
		ReadableStream<Uint8Array>
	>;
	const early: Reader[] = [];

	let reader: Reader;
	try {
		for (;;) {
			const next = await uniReader.read();
			if (next.done) throw new Error("no incoming uni stream for SETUP");

			const stream = new Reader(next.value, undefined, version);
			// A stream that ended or reset before its full type is that stream's failure, not the session's.
			// Malformed bytes are still the peer's fault, so a bad type fails the handshake.
			if (await stream.done().catch(() => true)) continue;
			// Full width, so an unknown type past 2^53 is held for the classifier to refuse.
			const streamType = await stream.peekU62().catch((err: unknown) => {
				if (err instanceof StreamError || err instanceof UnexpectedEnd) return undefined;
				throw err;
			});
			if (streamType === undefined) continue;
			if (streamType === BigInt(Ietf.Setup.id)) {
				reader = stream;
				break;
			}
			early.push(stream);
		}
	} finally {
		uniReader.releaseLock();
	}

	await reader.u53(); // the SETUP type, peeked above
	const setup = await Ietf.Setup.decode(reader, version);

	return {
		reader,
		early,
		solicit: Ietf.solicitFromSetup(setup.parameters),
		hidden: Ietf.hiddenFromSetup(setup.parameters),
		cluster: Ietf.Cluster.fromSetup(setup.parameters, version),
	};
}
