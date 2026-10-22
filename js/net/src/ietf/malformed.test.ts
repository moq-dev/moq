import { expect, spyOn, test } from "bun:test";
import { offered } from "../connection/extensions.ts";
import { exchangeSetup } from "../connection/handshake.ts";
import { ProtocolViolation, SessionCode } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import { Connection } from "./connection.ts";
import { GoAway } from "./goaway.ts";
import { RequestOk } from "./request.ts";
import { Setup } from "./setup.ts";
import { SetupOption, SetupOptions } from "./setup_options.ts";
import { SubscribeOk } from "./subscribe.ts";
import { SUBSCRIBE_TRACKS_ID, SubscribeNamespace } from "./subscribe_namespace.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

// Malformed control input the draft says ends the session must end it, with the draft's code.

const DRAFTS = [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_21, ALPN.DRAFT_21],
] as const;

/** A leading-ones varint, enough for the lengths below. */
function varint(n: number): number[] {
	if (n < 0x80) return [n];
	if (n < 0x4000) return [0x80 | (n >> 8), n & 0xff];
	throw new Error("varint too large for this helper");
}

/** A control message: type, u16 length, body. */
function frame(type: number, body: number[]): Uint8Array {
	return new Uint8Array([...varint(type), body.length >> 8, body.length & 0xff, ...body]);
}

/** A tuple field of `size` bytes. */
function field(size: number): number[] {
	return [...varint(size), ...new Array(size).fill(0x61)];
}

/** A SUBSCRIBE whose namespace tuple is `fields` and track name is `name` bytes. */
function subscribe(fields: number[][], name: number, requestId = 0): Uint8Array {
	return frame(0x03, [requestId, fields.length, ...fields.flat(), ...field(name), 0]);
}

/** Resolves to the session's close code, or undefined if it stays open a moment. */
async function closeCode(quic: WebTransport): Promise<number | undefined> {
	const open = Symbol("open");
	const result = await Promise.race([quic.closed, Bun.sleep(50).then(() => open)]);
	return result === open ? undefined : (result as WebTransportCloseInfo).closeCode;
}

/** A server publishing `room/video`, and a fresh client request stream. */
async function serving(version: IetfVersion, alpn: string) {
	const pair = createMockTransportPair(alpn);
	const control = await Stream.open(pair.server, { version });
	const origin = new OriginProducer();
	const broadcast = origin.createBroadcast(Path.from("room"));
	broadcast.announce();
	broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		maxRequestId: 100n,
		version,
		client: false,
		publish: origin.consume(),
	});
	const stream = await Stream.open(pair.client, { version });
	const close = () => {
		connection.abort();
		origin.close();
	};
	return { pair, stream, close };
}

/** A client session whose setup stream the test writes as the server. */
async function client(version: IetfVersion, alpn: string) {
	const pair = createMockTransportPair(alpn);
	const control = await Stream.open(pair.client, { version });
	const peer = await Stream.accept(pair.server, version);
	if (!peer) throw new Error("no setup stream");
	const connection = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.client,
		control,
		maxRequestId: 100n,
		version,
		client: true,
	});
	return { pair, peer, connection };
}

/**
 * `count` GOAWAYs in one write, so the session cannot reset the stream partway through. The
 * Request ID is draft-18 only, and only on the setup stream.
 */
function goaways(count: number, version: IetfVersion, requestId?: number): Uint8Array {
	const id = version === Version.DRAFT_18 && requestId !== undefined ? varint(requestId) : [];
	return new Uint8Array(new Array(count).fill([...frame(GoAway.id, [0, 0, ...id])]).flat());
}

for (const [version, alpn] of DRAFTS) {
	for (const [name, bytes, code] of [
		["a wrong-parity Request ID", subscribe([field(4)], 5, 1), SessionCode.InvalidRequestId],
		["an empty namespace field", subscribe([field(0)], 5), SessionCode.ProtocolViolation],
		["a namespace over 4096 bytes", subscribe([field(4097)], 1), SessionCode.ProtocolViolation],
		["a full track name over 4096 bytes", subscribe([field(4000)], 97), SessionCode.ProtocolViolation],
		["a trailing byte", frame(0x03, [0, 1, ...field(4), ...field(5), 0, 0]), SessionCode.ProtocolViolation],
		[
			"an invalid SUBSCRIBE_TRACKS FORWARD",
			frame(SUBSCRIBE_TRACKS_ID, [0, 0, 1, 0x10, 2]),
			SessionCode.ProtocolViolation,
		],
		[
			"a duplicate SUBSCRIBE_TRACKS FORWARD",
			frame(SUBSCRIBE_TRACKS_ID, [0, 0, 2, 0x10, 0, 0, 1]),
			SessionCode.ProtocolViolation,
		],
		[
			"a SUBSCRIBE_TRACKS prefix over 32 fields",
			frame(SUBSCRIBE_TRACKS_ID, [0, 33, ...new Array(33).fill(field(1)).flat(), 0]),
			SessionCode.ProtocolViolation,
		],
		[
			"a SUBSCRIBE_TRACKS prefix over 4096 bytes",
			frame(SUBSCRIBE_TRACKS_ID, [0, 1, ...field(4097), 0]),
			SessionCode.ProtocolViolation,
		],
	] as const) {
		test(`a request with ${name} closes ${alpn}`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => undefined);
			const { pair, stream, close } = await serving(version, alpn);
			try {
				await stream.writer.write(bytes);
				expect(await closeCode(pair.client)).toBe(code);
			} finally {
				logged.mockRestore();
				close();
			}
		});
	}

	test(`a request truncated at FIN closes ${alpn}`, async () => {
		const logged = spyOn(console, "error").mockImplementation(() => undefined);
		const { pair, stream, close } = await serving(version, alpn);
		try {
			await stream.writer.write(subscribe([field(4)], 5).slice(0, 6));
			stream.writer.close();
			expect(await closeCode(pair.client)).toBe(SessionCode.ProtocolViolation);
		} finally {
			logged.mockRestore();
			close();
		}
	});

	// A first GOAWAY on a request stream is legal; a second, or an unknown message, is not.
	for (const [name, count, code] of [
		["one GOAWAY", 1, undefined],
		["a second GOAWAY", 2, SessionCode.ProtocolViolation],
	] as const) {
		test(`${name} on a namespace subscription ${code === undefined ? "keeps" : "closes"} ${alpn}`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => undefined);
			const { pair, stream, close } = await serving(version, alpn);
			try {
				await stream.writer.u53(SubscribeNamespace.id);
				await new SubscribeNamespace({ requestId: 0n, namespace: Path.empty() }).encode(stream.writer, version);
				expect(await stream.reader.u53()).toBe(RequestOk.id);
				await stream.writer.write(goaways(count, version));
				expect(await closeCode(pair.client)).toBe(code);
			} finally {
				logged.mockRestore();
				close();
			}
		});

		test(`${name} on a subscription ${code === undefined ? "keeps" : "closes"} ${alpn}`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => undefined);
			const { pair, stream, close } = await serving(version, alpn);
			try {
				await stream.writer.write(
					frame(0x03, [
						0,
						1,
						4,
						...new TextEncoder().encode("room"),
						5,
						...new TextEncoder().encode("video"),
						0,
					]),
				);
				expect(await stream.reader.u53()).toBe(SubscribeOk.id);
				await stream.writer.write(goaways(count, version));
				expect(await closeCode(pair.client)).toBe(code);
			} finally {
				logged.mockRestore();
				close();
			}
		});
	}

	test(`an unknown message on a subscription closes ${alpn}`, async () => {
		const logged = spyOn(console, "error").mockImplementation(() => undefined);
		const { pair, stream, close } = await serving(version, alpn);
		try {
			await stream.writer.write(
				frame(0x03, [0, 1, 4, ...new TextEncoder().encode("room"), 5, ...new TextEncoder().encode("video"), 0]),
			);
			expect(await stream.reader.u53()).toBe(SubscribeOk.id);
			await stream.writer.write(frame(0x7e, []));
			expect(await closeCode(pair.client)).toBe(SessionCode.ProtocolViolation);
		} finally {
			logged.mockRestore();
			close();
		}
	});

	// The setup stream carries one GOAWAY and nothing else.
	for (const [name, write, code] of [
		["one GOAWAY", (w: Writer) => w.write(goaways(1, version, 0)), undefined],
		["a second GOAWAY", (w: Writer) => w.write(goaways(2, version, 0)), SessionCode.ProtocolViolation],
		["an unknown message", (w: Writer) => w.write(frame(0x7e, [])), SessionCode.ProtocolViolation],
		[
			"a GOAWAY URI over 8192 bytes",
			(w: Writer) => w.write(frame(GoAway.id, [...field(8193), 0, 0])),
			SessionCode.ProtocolViolation,
		],
		[
			"a GOAWAY truncated at FIN",
			async (w: Writer) => {
				await w.write(frame(GoAway.id, [0, 0]).slice(0, 3));
				w.close();
			},
			SessionCode.ProtocolViolation,
		],
	] as const) {
		test(`${name} on the setup stream ${code === undefined ? "keeps" : "closes"} ${alpn}`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => undefined);
			const { pair, peer, connection } = await client(version, alpn);
			try {
				await write(peer.writer);
				expect(await closeCode(pair.server)).toBe(code);
			} finally {
				logged.mockRestore();
				connection.abort();
			}
		});
	}

	// A server sends neither PATH nor AUTHORITY, and a token that cannot be decoded closes.
	for (const [name, key, value, code] of [
		["PATH", SetupOption.Path, [0x2f], SessionCode.InvalidPath],
		["AUTHORITY", SetupOption.Authority, [0x61], SessionCode.InvalidAuthority],
		["a malformed token", SetupOption.AuthorizationToken, [0x04], SessionCode.KeyValueFormatting],
		["a REGISTER token", SetupOption.AuthorizationToken, [0x01, 0x07, 0x00], undefined],
	] as const) {
		test(`a server SETUP with ${name} ${code === undefined ? "is accepted" : "closes"} ${alpn}`, async () => {
			const pair = createMockTransportPair(alpn);
			const setup = new Writer(await pair.server.createUnidirectionalStream(), version);
			await setup.u53(Setup.id);
			const parameters = new SetupOptions();
			parameters.setBytes(key, new Uint8Array(value));
			await new Setup({ parameters }).encode(setup, version);

			const dialed = exchangeSetup(pair.client, version, "test", { client: true, extensions: offered() });
			if (code === undefined) {
				await dialed;
				pair.client.close();
				return;
			}
			await expect(dialed).rejects.toBeInstanceOf(ProtocolViolation);
			expect((await pair.server.closed).closeCode).toBe(code);
		});
	}
}

// Draft-18's GOAWAY Request ID names the receiver's IDs: even for a client. It is required on
// the control stream.
for (const [requestId, code] of [
	[0, undefined],
	[4, undefined],
	[1, SessionCode.InvalidRequestId],
	[5, SessionCode.InvalidRequestId],
	[undefined, SessionCode.ProtocolViolation],
] as const) {
	const name = requestId === undefined ? "without a Request ID" : `Request ID ${requestId}`;
	test(`a draft-18 GOAWAY ${name} ${code === undefined ? "keeps" : "closes"} a client`, async () => {
		const logged = spyOn(console, "error").mockImplementation(() => undefined);
		const { pair, peer, connection } = await client(Version.DRAFT_18, ALPN.DRAFT_18);
		try {
			await peer.writer.write(goaways(1, Version.DRAFT_18, requestId));
			expect(await closeCode(pair.server)).toBe(code);
		} finally {
			logged.mockRestore();
			connection.abort();
		}
	});
}

// Draft-18's GOAWAY Request ID belongs on the control stream only.
for (const [version, alpn] of DRAFTS) {
	test(`a client's request stream GOAWAY naming a new session closes ${alpn}`, async () => {
		const logged = spyOn(console, "error").mockImplementation(() => undefined);
		const { pair, stream, close } = await serving(version, alpn);
		try {
			await stream.writer.u53(SubscribeNamespace.id);
			await new SubscribeNamespace({ requestId: 0n, namespace: Path.empty() }).encode(stream.writer, version);
			expect(await stream.reader.u53()).toBe(RequestOk.id);
			// New Session URI "a", then a zero timeout.
			await stream.writer.write(frame(GoAway.id, [1, 0x61, 0]));
			expect(await closeCode(pair.client)).toBe(SessionCode.ProtocolViolation);
		} finally {
			logged.mockRestore();
			close();
		}
	});
}

test("a draft-18 request stream GOAWAY with a Request ID closes", async () => {
	const logged = spyOn(console, "error").mockImplementation(() => undefined);
	const { pair, stream, close } = await serving(Version.DRAFT_18, ALPN.DRAFT_18);
	try {
		await stream.writer.u53(SubscribeNamespace.id);
		await new SubscribeNamespace({ requestId: 0n, namespace: Path.empty() }).encode(
			stream.writer,
			Version.DRAFT_18,
		);
		expect(await stream.reader.u53()).toBe(RequestOk.id);
		await stream.writer.write(goaways(1, Version.DRAFT_18, 0));
		expect(await closeCode(pair.client)).toBe(SessionCode.ProtocolViolation);
	} finally {
		logged.mockRestore();
		close();
	}
});
