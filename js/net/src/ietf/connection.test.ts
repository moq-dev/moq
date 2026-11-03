import { expect, spyOn, test } from "bun:test";
import { offered } from "../connection/extensions.ts";
import { exchangeSetup } from "../connection/handshake.ts";
import { SessionCode } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import { Connection } from "./connection.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Group } from "./object.ts";
import { RequestError, RequestOk } from "./request.ts";
import { Setup } from "./setup.ts";
import { SetupOption, SetupOptions } from "./setup_options.ts";
import { SubscribeOk } from "./subscribe.ts";
import { SUBSCRIBE_TRACKS_ID, SubscribeNamespace, SubscribeNamespaceEntry } from "./subscribe_namespace.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const PADDING = 0x132b3e28n;

/** A server-side session over `version`, and a client uni stream that sends only `type`. */
async function openUni(
	version: IetfVersion,
	alpn: string,
	type: bigint,
): Promise<{ pair: ReturnType<typeof createMockTransportPair>; connection: Connection; writer: Writer }> {
	const pair = createMockTransportPair(alpn);
	const control = await Stream.open(pair.server, { version });
	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		version,
		client: false,
	});

	const writer = new Writer(await pair.client.createUnidirectionalStream(), version);
	await writer.u62(type);

	return { pair, connection, writer };
}

/** Padding is read to the end and dropped: no STOP_SENDING, and the session stays up. */
test("a padding stream is discarded", async () => {
	const { pair, connection, writer } = await openUni(Version.DRAFT_18, ALPN.DRAFT_18, PADDING);
	let closed = false;
	void pair.server.closed.then(() => {
		closed = true;
	});

	try {
		await writer.write(new Uint8Array(4096));
		writer.close();

		// A STOP_SENDING would reject this; a clean close means every byte was read.
		await writer.closed;

		// A session close would come from the same handler that read the stream.
		await Bun.sleep(0);
		expect(closed).toBe(false);
	} finally {
		connection.abort();
	}
});

/** An unknown or invalid stream type MUST close the session with PROTOCOL_VIOLATION. */
test("an unknown uni stream type closes the session", async () => {
	for (const [version, alpn, type] of [
		[Version.DRAFT_18, ALPN.DRAFT_18, 0n],
		// Past 2^53, so it cannot be read as a number.
		[Version.DRAFT_18, ALPN.DRAFT_18, 2n ** 53n],
		// A SUBGROUP_HEADER with the reserved SUBGROUP_ID_MODE (0b11).
		[Version.DRAFT_18, ALPN.DRAFT_18, 0x56n],
	] as const) {
		const logged = spyOn(console, "error").mockImplementation(() => void 0);
		const { pair, connection } = await openUni(version, alpn, type);

		try {
			const info = await pair.client.closed;
			expect(info.closeCode).toBe(SessionCode.ProtocolViolation);
		} finally {
			logged.mockRestore();
			connection.abort();
		}
	}
});

/** Unknown bidi message types close the session, including full-width varints. */
for (const [version, alpn] of [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const) {
	for (const type of [0n, 2n ** 53n]) {
		test(`unknown bidi type 0x${type.toString(16)} closes ${alpn}`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => void 0);
			const pair = createMockTransportPair(alpn);
			const control = await Stream.open(pair.server, { version });
			const connection = new Connection({
				url: new URL("https://example.com"),
				quic: pair.server,
				control,
				version,
				client: false,
			});

			try {
				const stream = await Stream.open(pair.client, { version });
				await stream.writer.u62(type);

				const info = await pair.client.closed;
				expect(info.closeCode).toBe(SessionCode.ProtocolViolation);
			} finally {
				logged.mockRestore();
				connection.abort();
			}
		});
	}
}

/** We list no tracks by PUBLISH: each SUBSCRIBE_TRACKS gets NOT_SUPPORTED, and the session stays usable. */
for (const [version, alpn] of [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_20, ALPN.DRAFT_20],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const) {
	test(`SUBSCRIBE_TRACKS is refused on its own stream on ${alpn}`, async () => {
		const pair = createMockTransportPair(alpn);
		const control = await Stream.open(pair.server, { version });
		const connection = new Connection({
			url: new URL("https://example.com"),
			quic: pair.server,
			control,
			version,
			client: false,
		});

		try {
			// A second listing of the same prefix is refused the same way, each spending its Request ID.
			for (let i = 0; i < 2; i++) {
				const stream = await Stream.open(pair.client, { version });
				await stream.writer.u62(BigInt(SUBSCRIBE_TRACKS_ID));
				await Message.encode(stream.writer, async (writer) => {
					await writer.u62(BigInt(i * 2));
					await Namespace.encode(writer, Path.from("prefix"));
					await writer.u53(0);
				});
				expect(await stream.reader.u53()).toBe(RequestError.id);
				expect((await RequestError.decode(stream.reader, version)).errorCode).toBe(0x3);
				expect(await stream.reader.done()).toBe(true);
			}
		} finally {
			connection.abort();
		}
	});
}

/** A SUBSCRIBE_NAMESPACE always asks for namespaces, so it gets the ones we publish. */
test("SUBSCRIBE_NAMESPACE gets the published namespaces", async () => {
	const version = Version.DRAFT_18;
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const control = await Stream.open(pair.server, { version });
	const origin = new OriginProducer();
	origin.createBroadcast(Path.from("room")).announce();
	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		version,
		client: false,
		publish: origin.consume(),
		solicit: true,
	});

	try {
		const stream = await Stream.open(pair.client, { version });
		await stream.writer.u53(SubscribeNamespace.id);
		await new SubscribeNamespace({ namespace: Path.empty(), requestId: 0n }).encode(stream.writer, version);

		expect(await stream.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(stream.reader, version);
		expect(await stream.reader.u53()).toBe(SubscribeNamespaceEntry.id);
		expect((await SubscribeNamespaceEntry.decode(stream.reader, version)).suffix).toBe(Path.from("room"));
	} finally {
		connection.abort();
		origin.close();
	}
});

/**
 * Padding and group streams that beat the peer's SETUP are held and classified once it
 * lands. The drafts say to buffer early data; failing the handshake over it broke a peer
 * that already held a subscription or probed for bandwidth straight away.
 */
test("uni streams before SETUP are held until it lands", async () => {
	const version = Version.DRAFT_18;
	const pair = createMockTransportPair(ALPN.DRAFT_18);

	const padding = new Writer(await pair.client.createUnidirectionalStream(), version);
	await padding.u62(PADDING);
	await padding.write(new Uint8Array(4096));
	padding.close();

	const group = new Writer(await pair.client.createUnidirectionalStream(), version);
	await new Group({
		trackAlias: 7n,
		groupId: 0,
		subGroupId: 0,
		publisherPriority: 128,
		flags: {
			hasExtensions: false,
			hasSubgroup: false,
			hasSubgroupObject: false,
			hasEnd: false,
			hasPriority: true,
			firstObject: true,
		},
	}).encode(group, version);

	const setup = new Writer(await pair.client.createUnidirectionalStream(), version);
	await setup.u53(Setup.id);
	const parameters = new SetupOptions();
	parameters.setBytes(SetupOption.Implementation, new TextEncoder().encode("test"));
	await new Setup({ parameters }).encode(setup, version);

	const { control, early, solicit, hidden, cluster } = await exchangeSetup(pair.server, version, "test", {
		client: false,
		extensions: offered(),
	});
	expect(early.length).toBe(2);

	let closed = false;
	void pair.server.closed.then(() => {
		closed = true;
	});

	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		early,
		solicit,
		hidden,
		cluster,
		version,
		client: false,
	});

	try {
		// A STOP_SENDING would reject this; a clean close means the padding was read to its end.
		await padding.closed;

		await Bun.sleep(0);
		expect(closed).toBe(false);
	} finally {
		connection.abort();
	}
});

/** An early stream that ends partway through its type died; it is skipped like an empty one. */
test("truncated uni stream before SETUP is skipped", async () => {
	const version = Version.DRAFT_18;
	const pair = createMockTransportPair(ALPN.DRAFT_18);

	// A two-byte varint prefix, then FIN.
	const truncated = new Writer(await pair.client.createUnidirectionalStream(), version);
	await truncated.write(new Uint8Array([0x80]));
	truncated.close();

	const setup = new Writer(await pair.client.createUnidirectionalStream(), version);
	await setup.u53(Setup.id);
	const parameters = new SetupOptions();
	parameters.setBytes(SetupOption.Implementation, new TextEncoder().encode("test"));
	await new Setup({ parameters }).encode(setup, version);

	const { early } = await exchangeSetup(pair.server, version, "test", { client: false, extensions: offered() });
	expect(early.length).toBe(0);
});

for (const [version, alpn] of [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const) {
	for (const [value, nested] of [
		[0, false],
		[3, false],
		[255, false],
		[0, true],
		[3, true],
		[255, true],
	] as const) {
		if (nested && version === Version.DRAFT_18) continue; // FILL_PARAMETERS starts in draft-22.
		test(`invalid ${nested ? "fill " : ""}GROUP_ORDER ${value} closes ${alpn}`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => void 0);
			const pair = createMockTransportPair(alpn);
			const control = await Stream.open(pair.server, { version });
			const connection = new Connection({
				url: new URL("https://example.com"),
				quic: pair.server,
				control,
				version,
				client: false,
			});
			try {
				const stream = await Stream.open(pair.client, { version });
				await stream.writer.u53(3);
				const params = nested ? [0x23, 3, 1, 0x22, value] : [0x22, value];
				const body = [0, 1, 1, 97, 1, 98, 1, ...params];
				await stream.writer.write(new Uint8Array([0, body.length, ...body]));
				const info = await pair.client.closed;
				expect(info.closeCode).toBe(SessionCode.ProtocolViolation);
			} finally {
				logged.mockRestore();
				connection.abort();
			}
		});
	}
}

/** A parameter block of AUTHORIZATION TOKENs. Each key is a delta from the one before, so a repeat is 0. */
function tokenParams(...tokens: (readonly number[])[]): number[] {
	return [tokens.length, ...tokens.flatMap((token, i) => [i > 0 ? 0 : 0x03, token.length, ...token])];
}

/** A SUBSCRIBE for `room/video` whose parameter block is `params`. */
function subscribeWith(params: readonly number[], requestId = 0): Uint8Array {
	const ascii = (text: string) => [...new TextEncoder().encode(text)];
	// Request ID, then the namespace and track name.
	const body = [requestId, 1, 4, ...ascii("room"), 5, ...ascii("video"), ...params];
	return new Uint8Array([0x03, 0, body.length, ...body]);
}

/** A SUBSCRIBE for `room/video` whose only parameter is an AUTHORIZATION TOKEN holding `token`. */
function subscribeWithToken(token: readonly number[]): Uint8Array {
	return subscribeWith(tokenParams(token));
}

/** An update to that SUBSCRIBE whose only parameters are AUTHORIZATION TOKENs holding `tokens`. */
function updateWithTokens(...tokens: (readonly number[])[]): Uint8Array {
	// Its own Request ID, which must be fresh: the subscription already spent 0.
	const body = [2, ...tokenParams(...tokens)];
	return new Uint8Array([0x02, 0, body.length, ...body]);
}

/**
 * A request's AUTHORIZATION TOKEN decodes by the SETUP option's rules, so one this endpoint
 * cannot use closes the session with the draft's code.
 */
for (const [version, alpn] of [[Version.DRAFT_22, ALPN.DRAFT_22]] as const) {
	for (const [name, token, code] of [
		["REGISTER", [0x01, 0x07, 0x00], SessionCode.AuthTokenCacheOverflow],
		["DELETE", [0x00, 0x07], SessionCode.UnknownAuthTokenAlias],
		["USE_ALIAS", [0x02, 0x07], SessionCode.UnknownAuthTokenAlias],
		["USE_VALUE without a type", [0x03], SessionCode.KeyValueFormatting],
		["an unknown alias type", [0x04, 0x00], SessionCode.KeyValueFormatting],
	] as const) {
		test(`a request token with ${name} closes ${alpn}`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => void 0);
			const pair = createMockTransportPair(alpn);
			const control = await Stream.open(pair.server, { version });
			const connection = new Connection({
				url: new URL("https://example.com"),
				quic: pair.server,
				control,
				version,
				client: false,
			});
			try {
				const stream = await Stream.open(pair.client, { version });
				if (!stream) throw new Error("no control stream");
				await stream.writer.write(subscribeWithToken(token));

				const info = await pair.client.closed;
				expect(info.closeCode).toBe(code);
			} finally {
				logged.mockRestore();
				connection.abort();
			}
		});
	}
}

/**
 * An update's AUTHORIZATION TOKEN decodes like the request's, so one this endpoint cannot use
 * closes the session with the draft's code, not only the subscription it updates.
 */
for (const [version, alpn] of [[Version.DRAFT_22, ALPN.DRAFT_22]] as const) {
	for (const [name, token, code] of [
		["REGISTER", [0x01, 0x07, 0x00], SessionCode.AuthTokenCacheOverflow],
		["DELETE", [0x00, 0x07], SessionCode.UnknownAuthTokenAlias],
		["USE_ALIAS", [0x02, 0x07], SessionCode.UnknownAuthTokenAlias],
		["USE_VALUE without a type", [0x03], SessionCode.KeyValueFormatting],
	] as const) {
		test(`an update token with ${name} closes ${alpn}`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => void 0);
			const pair = createMockTransportPair(alpn);
			const control = await Stream.open(pair.server, { version });
			const origin = new OriginProducer();
			const broadcastPublication = origin.createBroadcast(Path.from("room"));
			const broadcast = broadcastPublication.broadcast;
			broadcastPublication.announce();
			broadcast.createTrack("video", { timescale: Timescale.MILLI });
			const connection = new Connection({
				url: new URL("https://example.com"),
				quic: pair.server,
				control,
				version,
				client: false,
				publish: origin.consume(),
			});
			try {
				const stream = await Stream.open(pair.client, { version });
				if (!stream) throw new Error("no control stream");
				await stream.writer.write(subscribeWith([0]));
				expect(await stream.reader.u53()).toBe(SubscribeOk.id);
				await SubscribeOk.decode(stream.reader, version);
				await stream.writer.write(updateWithTokens(token));

				const info = await pair.client.closed;
				expect(info.closeCode).toBe(code);
			} finally {
				logged.mockRestore();
				connection.abort();
				origin.close();
			}
		});
	}
}

/** A server publishing `room/video`, and the client's stream to send requests on. */
async function serving(version: IetfVersion, alpn: string) {
	const pair = createMockTransportPair(alpn);
	const control = await Stream.open(pair.server, { version });
	const origin = new OriginProducer();
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		version,
		client: false,
		publish: origin.consume(),
	});
	const stream = await Stream.open(pair.client, { version });
	if (!stream) throw new Error("no control stream");
	const close = () => {
		connection.abort();
		origin.close();
	};
	return { pair, stream, close };
}

/** USE_VALUE, type 0, value 0xAA. */
const TOKEN = [0x03, 0x00, 0xaa] as const;

/**
 * Section 8.9: a request whose tokens repeat is rejected MALFORMED_AUTH_TOKEN, and the session
 * stays open for the next request.
 */
for (const [version, alpn] of [[Version.DRAFT_22, ALPN.DRAFT_22]] as const) {
	test(`a repeated request token rejects only the request ${alpn}`, async () => {
		const { pair, stream, close } = await serving(version, alpn);
		try {
			await stream.writer.write(subscribeWith(tokenParams(TOKEN, TOKEN)));
			expect(await stream.reader.u53()).toBe(RequestError.id);
			expect((await RequestError.decode(stream.reader, version)).errorCode).toBe(0x4);

			const next = await Stream.open(pair.client, { version });
			await next.writer.write(subscribeWith(tokenParams(TOKEN, [0x03, 0x00, 0xbb]), 2));
			expect(await next.reader.u53()).toBe(SubscribeOk.id);
			await SubscribeOk.decode(next.reader, version);
		} finally {
			close();
		}
	});
}

/** An update whose tokens repeat is answered MALFORMED_AUTH_TOKEN, not NOT_SUPPORTED. */
for (const [version, alpn] of [[Version.DRAFT_22, ALPN.DRAFT_22]] as const) {
	test(`a repeated update token is malformed ${alpn}`, async () => {
		const { stream, close } = await serving(version, alpn);
		try {
			await stream.writer.write(subscribeWith([0]));
			expect(await stream.reader.u53()).toBe(SubscribeOk.id);
			await SubscribeOk.decode(stream.reader, version);
			await stream.writer.write(updateWithTokens(TOKEN, TOKEN));
			expect(await stream.reader.u53()).toBe(RequestError.id);
			const error = await RequestError.decode(stream.reader, version);
			expect(error.errorCode).toBe(0x4);
		} finally {
			close();
		}
	});
}
