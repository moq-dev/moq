import { Milli } from "@moq/clock";
import { closeReason, ProtocolViolation, SessionCode, StreamError } from "../error.ts";
import { type Hop, randomHop } from "../hop.ts";
import * as Ietf from "../ietf/index.ts";
import * as Message from "../ietf/message.ts";
import * as Lite from "../lite/index.ts";
import { acceptBudget, Reader, Stream, UnexpectedEnd, Writer } from "../stream.ts";
import * as Extensions from "./extensions.ts";

/** Default bound on a connection attempt, including its SETUP exchange. @internal */
export const DEFAULT_TIMEOUT = Milli(30_000);

/**
 * Draft-18+ SETUP exchange. Each side opens a uni stream, writes its Setup
 * message, and reads the peer's Setup off an incoming uni stream. The two
 * halves run in parallel and the protocol is symmetric, so both `connect`
 * (client) and `accept` (server) use this same function.
 *
 * Returns the control stream, the uni streams that arrived before it (see
 * {@link receiveSetup}), plus what the peer's SETUP declared: whether it requires
 * solicitation, which decides whether we announce namespaces unprompted (see the MoQ Solicit
 * extension), its Hop ID (see the MoQ Cluster extension), and whether MoQ Auth and MoQ Epoch
 * are negotiated, plus the implementation it advertised. We declare our Hop ID on every
 * session, and Solicit, Auth, and Epoch unless `extensions` declines them: we send SUBSCRIBE_NAMESPACE for each prefix we want, so an unsolicited advertisement
 * can tell us nothing we won't have asked for, and a peer that knows our Hop ID can withhold
 * the advertisements that already flowed through us.
 */
export async function exchangeSetup(
	transport: WebTransport,
	version: Ietf.IetfVersion,
	implementation: string,
	{ client, extensions, token }: { client: boolean; extensions: Extensions.Offered; token?: Uint8Array },
): Promise<{
	control: Stream;
	early: Reader[];
	solicit: boolean | undefined;
	hidden: boolean;
	maxAge: boolean;
	activeCount: boolean;
	cluster: Ietf.Cluster.Hops;
	auth: boolean;
	epoch: boolean;
	peerImplementation: string | undefined;
}> {
	const params = new Ietf.SetupOptions();
	Ietf.implementationIntoSetup(params, implementation);
	Extensions.intoSetup(params, extensions);
	Ietf.hiddenIntoSetup(params);
	Ietf.maxAgeIntoSetup(params);
	Ietf.activeCountIntoSetup(params);
	// Only a client presents a connection credential.
	if (client && token) Ietf.tokenIntoSetup(params, { kind: Ietf.TOKEN_OUT_OF_BAND, value: token });

	// One id per session, like the moq-lite connection: nothing in this process forwards
	// between sessions, so there is nothing for a shared id to detect.
	const self = randomHop();
	Ietf.Cluster.intoSetup(params, self);

	const setupMsg = new Ietf.Setup({ parameters: params });

	let writer: Writer;
	let received: Awaited<ReturnType<typeof receiveSetup>>;
	try {
		[writer, received] = await Promise.all([
			sendSetup(transport, version, setupMsg),
			receiveSetup(transport, version, client),
		]);
	} catch (err) {
		// A SETUP we refuse closes with the code the draft names for it.
		if (err instanceof ProtocolViolation)
			transport.close({ closeCode: err.code, reason: closeReason(err.message) });
		throw err;
	}

	return {
		control: new Stream({ writer, reader: received.reader }),
		early: received.early,
		solicit: received.solicit,
		hidden: received.hidden,
		maxAge: received.maxAge,
		activeCount: received.activeCount,
		cluster: { self, peer: received.cluster },
		// Negotiated only when both sides offer it.
		auth: received.auth && extensions.auth,
		epoch: received.epoch && extensions.epoch,
		peerImplementation: received.implementation,
	};
}

async function sendSetup(transport: WebTransport, version: Ietf.IetfVersion, setupMsg: Ietf.Setup): Promise<Writer> {
	// Via Writer.open for its deadline: a peer can advertise a stream limit of zero and
	// never raise it, which would otherwise hang the handshake rather than failing it.
	const writer = await Writer.open(transport, { version });
	await Message.write(writer, Ietf.Setup.id, setupMsg, version); // 0x2F00 stream type
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
	client: boolean,
): Promise<{
	reader: Reader;
	early: Reader[];
	solicit: boolean | undefined;
	hidden: boolean;
	maxAge: boolean;
	activeCount: boolean;
	cluster: Hop | undefined;
	auth: boolean;
	epoch: boolean;
	implementation: string | undefined;
}> {
	const uniReader = transport.incomingUnidirectionalStreams.getReader() as ReadableStreamDefaultReader<
		ReadableStream<Uint8Array>
	>;
	const early: Reader[] = [];

	let reader: Reader;
	try {
		for (;;) {
			// Each stream racing ahead of SETUP is held, so a peer opening them at once must
			// not hold the host for its whole stream credit.
			const pause = acceptBudget.poll();
			if (pause !== undefined) await pause;
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

	// A server sends neither PATH nor AUTHORITY (draft-22 sections 9.1.1 and 9.1.2).
	if (client && setup.parameters.getBytes(Ietf.SetupOption.Path) !== undefined) {
		throw new ProtocolViolation("server SETUP carries PATH", { code: SessionCode.InvalidPath });
	}
	if (client && setup.parameters.getBytes(Ietf.SetupOption.Authority) !== undefined) {
		throw new ProtocolViolation("server SETUP carries AUTHORITY", { code: SessionCode.InvalidAuthority });
	}
	// Nothing here uses the peer's token, but one that cannot be decoded still closes the
	// session (draft-22 section 8.9).
	Ietf.tokenFromSetup(setup.parameters);

	return {
		reader,
		early,
		solicit: Ietf.solicitFromSetup(setup.parameters),
		hidden: Ietf.hiddenFromSetup(setup.parameters),
		maxAge: Ietf.maxAgeFromSetup(setup.parameters),
		activeCount: Ietf.activeCountFromSetup(setup.parameters),
		cluster: Ietf.Cluster.fromSetup(setup.parameters),
		auth: Ietf.Auth.fromSetup(setup.parameters) === true,
		epoch: Ietf.Epoch.fromSetup(setup.parameters),
		implementation: Ietf.implementationFromSetup(setup.parameters),
	};
}

/** Whether SETUP selecting `version` ends the SETUP stream, the session continuing as its ALPN would. */
export function handsOff(version: Lite.Version): boolean {
	return version !== Lite.Version.DRAFT_01 && version !== Lite.Version.DRAFT_02;
}
