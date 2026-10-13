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
import { Parameters, SetupOption, SetupOptions } from "./parameters.ts";
import { initialMaxRequestId, MaxRequestId, RequestError, RequestOk } from "./request.ts";
import { Setup } from "./setup.ts";
import { SubscribeError, SubscribeOk } from "./subscribe.ts";
import {
	SUBSCRIBE_TRACKS_ID,
	SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceLegacy,
	SubscribeOptions,
} from "./subscribe_namespace.ts";
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
		maxRequestId: 100n,
		version,
		client: false,
	});

	const writer = new Writer(await pair.client.createUnidirectionalStream(), version);
	await writer.u62(type);

	return { pair, connection, writer };
}

/** Padding is read to the end and dropped: no STOP_SENDING, and the session stays up. */
test("a padding stream is discarded", async () => {
	const { pair, connection, writer } = await openUni(Version.DRAFT_19, ALPN.DRAFT_19, PADDING);
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
		[Version.DRAFT_19, ALPN.DRAFT_19, 0n],
		// Past 2^53, so it cannot be read as a number.
		[Version.DRAFT_19, ALPN.DRAFT_19, 2n ** 53n],
		// Padding arrived in draft-18.
		[Version.DRAFT_17, ALPN.DRAFT_17, PADDING],
		// A SUBGROUP_HEADER with the reserved SUBGROUP_ID_MODE (0b11).
		[Version.DRAFT_19, ALPN.DRAFT_19, 0x56n],
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
	[Version.DRAFT_17, ALPN.DRAFT_17],
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_19, ALPN.DRAFT_19],
	[Version.DRAFT_20, ALPN.DRAFT_20],
	[Version.DRAFT_21, ALPN.DRAFT_21],
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
				maxRequestId: 100n,
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

/** A defined but unsupported request is refused without closing the session. */
test("SUBSCRIBE_TRACKS refuses only its request", async () => {
	const version = Version.DRAFT_19;
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const control = await Stream.open(pair.server, { version });
	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		maxRequestId: 100n,
		version,
		client: false,
	});

	try {
		// A second refusal proves the first left the session usable.
		for (let i = 0; i < 2; i++) {
			const stream = await Stream.open(pair.client, { version });
			await stream.writer.u62(BigInt(SUBSCRIBE_TRACKS_ID));
			await Message.encode(stream.writer, async (writer) => {
				await writer.u62(BigInt(i * 2));
				await Namespace.encode(writer, Path.from(""));
				await writer.u53(0);
			});
			expect(await stream.reader.u53()).toBe(RequestError.id);
			const refused = await RequestError.decode(stream.reader, version);
			expect(refused.errorCode).toBe(0x3);
			expect(refused.reasonPhrase).toBe("SUBSCRIBE_TRACKS is not supported");
		}
	} finally {
		connection.abort();
	}
});

/**
 * A draft-16/17 SUBSCRIBE_NAMESPACE gets NAMESPACE whenever it asks for namespaces, and one
 * asking for PUBLISH alone is refused, since we never send PUBLISH. Draft-18 has no
 * Subscribe Options and always asks for namespaces.
 */
for (const [version, alpn, options, namespaces] of [
	[Version.DRAFT_16, ALPN.DRAFT_16, SubscribeOptions.PUBLISH, false],
	[Version.DRAFT_16, ALPN.DRAFT_16, SubscribeOptions.NAMESPACE, true],
	[Version.DRAFT_16, ALPN.DRAFT_16, SubscribeOptions.BOTH, true],
	[Version.DRAFT_17, ALPN.DRAFT_17, SubscribeOptions.PUBLISH, false],
	[Version.DRAFT_17, ALPN.DRAFT_17, SubscribeOptions.NAMESPACE, true],
	[Version.DRAFT_17, ALPN.DRAFT_17, SubscribeOptions.BOTH, true],
	[Version.DRAFT_18, ALPN.DRAFT_18, undefined, true],
] as const) {
	test(`${alpn} SUBSCRIBE_NAMESPACE with Subscribe Options ${options ?? "absent"}`, async () => {
		const pair = createMockTransportPair(alpn);
		const control = await Stream.open(pair.server, { version });
		const origin = new OriginProducer();
		origin.createBroadcast(Path.from("room")).announce();
		const connection = new Connection({
			url: new URL("https://example.com"),
			quic: pair.server,
			control,
			maxRequestId: 100n,
			version,
			client: false,
			publish: origin.consume(),
			solicit: true,
		});

		try {
			const stream = await Stream.open(pair.client, { version });
			if (options === undefined) {
				await stream.writer.u53(SubscribeNamespace.id);
				await new SubscribeNamespace({ namespace: Path.empty(), requestId: 0n }).encode(stream.writer, version);
			} else {
				await stream.writer.u53(SubscribeNamespaceLegacy.id);
				await new SubscribeNamespaceLegacy({
					namespace: Path.empty(),
					requestId: 0n,
					subscribeOptions: options,
				}).encode(stream.writer, version);
			}

			if (namespaces) {
				expect(await stream.reader.u53()).toBe(RequestOk.id);
				await RequestOk.decode(stream.reader, version);
				expect(await stream.reader.u53()).toBe(SubscribeNamespaceEntry.id);
				expect((await SubscribeNamespaceEntry.decode(stream.reader, version)).suffix).toBe(Path.from("room"));
			} else {
				expect(await stream.reader.u53()).toBe(RequestError.id);
				const refused = await RequestError.decode(stream.reader, version);
				expect(refused.errorCode).toBe(0x3);
				expect(refused.requestId).toBe(version === Version.DRAFT_16 ? 0n : undefined);
				expect(await stream.reader.done()).toBe(true);
			}
		} finally {
			connection.abort();
			origin.close();
		}
	});
}

/** The drafts define only 0x00 through 0x02; any other value closes the session, even past 2^53. */
for (const [version, alpn] of [
	[Version.DRAFT_16, ALPN.DRAFT_16],
	[Version.DRAFT_17, ALPN.DRAFT_17],
] as const) {
	for (const options of [3n, 2n ** 53n]) {
		test(`${alpn} Subscribe Options ${options} closes the session`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => void 0);
			const pair = createMockTransportPair(alpn);
			const control = await Stream.open(pair.server, { version });
			const connection = new Connection({
				url: new URL("https://example.com"),
				quic: pair.server,
				control,
				maxRequestId: 100n,
				version,
				client: false,
			});

			try {
				const stream = await Stream.open(pair.client, { version });
				await stream.writer.u53(SubscribeNamespaceLegacy.id);
				await Message.encode(stream.writer, async (w) => {
					await w.u62(1n); // Request ID
					if (version === Version.DRAFT_17) await w.u62(0n); // Required Request ID delta
					await Namespace.encode(w, Path.empty());
					await w.u62(options);
					await new Parameters().encode(w, version);
				});

				const info = await pair.client.closed;
				expect(info.closeCode).toBe(SessionCode.ProtocolViolation);
			} finally {
				logged.mockRestore();
				connection.abort();
			}
		});
	}
}

/**
 * Padding and group streams that beat the peer's SETUP are held and classified once it
 * lands. The drafts say to buffer early data; failing the handshake over it broke a peer
 * that already held a subscription or probed for bandwidth straight away.
 */
test("uni streams before SETUP are held until it lands", async () => {
	const version = Version.DRAFT_19;
	const pair = createMockTransportPair(ALPN.DRAFT_19);

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

	const { control, early, solicit, hidden, cluster } = await exchangeSetup(pair.server, version, "test", offered());
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
		maxRequestId: 0n,
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

/** An early stream that dies before its type is skipped, but a malformed type is still fatal. */
test("malformed uni stream before SETUP fails the handshake", async () => {
	const version = Version.DRAFT_17;
	const pair = createMockTransportPair(ALPN.DRAFT_17);

	const empty = new Writer(await pair.client.createUnidirectionalStream(), version);
	empty.close();

	// 0b1111110x is a reserved varint prefix on draft-17.
	const malformed = new Writer(await pair.client.createUnidirectionalStream(), version);
	await malformed.write(new Uint8Array([0xfc, 0, 0, 0, 0, 0, 0, 0, 0]));

	const setup = new Writer(await pair.client.createUnidirectionalStream(), version);
	await setup.u53(Setup.id);
	const parameters = new SetupOptions();
	parameters.setBytes(SetupOption.Implementation, new TextEncoder().encode("test"));
	await new Setup({ parameters }).encode(setup, version);

	await expect(exchangeSetup(pair.server, version, "test", offered())).rejects.toThrow("leading-ones varint");
});

/** An early stream that ends partway through its type died; it is skipped like an empty one. */
test("truncated uni stream before SETUP is skipped", async () => {
	const version = Version.DRAFT_17;
	const pair = createMockTransportPair(ALPN.DRAFT_17);

	// A two-byte varint prefix, then FIN.
	const truncated = new Writer(await pair.client.createUnidirectionalStream(), version);
	await truncated.write(new Uint8Array([0x80]));
	truncated.close();

	const setup = new Writer(await pair.client.createUnidirectionalStream(), version);
	await setup.u53(Setup.id);
	const parameters = new SetupOptions();
	parameters.setBytes(SetupOption.Implementation, new TextEncoder().encode("test"));
	await new Setup({ parameters }).encode(setup, version);

	const { early } = await exchangeSetup(pair.server, version, "test", offered());
	expect(early.length).toBe(0);
});

// A PUBLISH-only request is refused, but it still spends an id and must give it back.
for (const [name, options] of [
	["namespaces", SubscribeOptions.NAMESPACE],
	["publish only", SubscribeOptions.PUBLISH],
] as const) {
	/** Draft-16 carries SUBSCRIBE_NAMESPACE on its own stream, so the control adapter never sees the id. */
	test(`draft-16 subscribe namespace for ${name} past the request window closes the session`, async () => {
		const logged = spyOn(console, "error").mockImplementation(() => undefined);
		const version = Version.DRAFT_16;
		const pair = createMockTransportPair(ALPN.DRAFT_16);
		const control = await Stream.open(pair.server, { version });
		const connection = new Connection({
			url: new URL("https://example.com"),
			quic: pair.server,
			control,
			maxRequestId: 100n,
			version,
			client: false,
		});

		try {
			const stream = await Stream.open(pair.client, { version });
			await stream.writer.u53(SubscribeNamespaceLegacy.id);
			await new SubscribeNamespaceLegacy({
				requestId: initialMaxRequestId(true),
				namespace: Path.from("room"),
				subscribeOptions: options,
			}).encode(stream.writer, version);

			const info = await pair.client.closed;
			expect(info.closeCode).toBe(SessionCode.TooManyRequests);
			expect(logged).not.toHaveBeenCalled();
		} finally {
			logged.mockRestore();
			connection.abort();
		}
	});

	/** Ending that stream has to grant another id, or the next namespace request stalls at the same ceiling. */
	test(`draft-16 subscribe namespace for ${name} grants another request id when it ends`, async () => {
		const logged = spyOn(console, "error").mockImplementation(() => undefined);
		const version = Version.DRAFT_16;
		const pair = createMockTransportPair(ALPN.DRAFT_16);
		const control = await Stream.open(pair.server, { version });
		const connection = new Connection({
			url: new URL("https://example.com"),
			quic: pair.server,
			control,
			maxRequestId: 100n,
			version,
			client: false,
			requestWindow: 1n,
		});
		const peerControl = await Stream.accept(pair.client, version);
		if (!peerControl) throw new Error("no control stream");

		try {
			const stream = await Stream.open(pair.client, { version });
			await stream.writer.u53(SubscribeNamespaceLegacy.id);
			await new SubscribeNamespaceLegacy({
				requestId: 0n,
				namespace: Path.from("room"),
				subscribeOptions: options,
			}).encode(stream.writer, version);

			if (options === SubscribeOptions.PUBLISH) {
				expect(await stream.reader.u53()).toBe(RequestError.id);
				expect((await RequestError.decode(stream.reader, version)).requestId).toBe(0n);
			} else {
				expect(await stream.reader.u53()).toBe(RequestOk.id);
				expect((await RequestOk.decode(stream.reader, version)).requestId).toBe(0n);
			}
			stream.close();

			expect(await peerControl.reader.u53()).toBe(MaxRequestId.id);
			const grant = await MaxRequestId.decode(peerControl.reader, version);
			expect(grant.requestId).toBe(initialMaxRequestId(true, 1n) + 2n);
			expect(logged).not.toHaveBeenCalled();
		} finally {
			logged.mockRestore();
			connection.abort();
		}
	});
}

for (const [version, alpn] of [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_21, ALPN.DRAFT_21],
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
		if (nested && version === Version.DRAFT_18) continue; // FILL_PARAMETERS starts in draft-20.
		test(`invalid ${nested ? "fill " : ""}GROUP_ORDER ${value} closes ${alpn}`, async () => {
			const logged = spyOn(console, "error").mockImplementation(() => void 0);
			const pair = createMockTransportPair(alpn);
			const control = await Stream.open(pair.server, { version });
			const connection = new Connection({
				url: new URL("https://example.com"),
				quic: pair.server,
				control,
				maxRequestId: 100n,
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

/**
 * A parameter block of AUTHORIZATION TOKENs. Draft-16 on codes each key as a delta from the
 * one before, so a repeat is 0.
 */
function tokenParams(version: IetfVersion, ...tokens: (readonly number[])[]): number[] {
	const delta = version !== Version.DRAFT_14 && version !== Version.DRAFT_15;
	return [tokens.length, ...tokens.flatMap((token, i) => [i > 0 && delta ? 0 : 0x03, token.length, ...token])];
}

/** A SUBSCRIBE for `room/video` whose parameter block is `params`. */
function subscribeWith(version: IetfVersion, params: readonly number[], requestId = 0): Uint8Array {
	const ascii = (text: string) => [...new TextEncoder().encode(text)];
	// Request ID, then the namespace and track name.
	const head = [requestId, 1, 4, ...ascii("room"), 5, ...ascii("video")];
	// Draft-14 carries priority, group order, forward, and a Largest Object filter inline.
	const fields = version === Version.DRAFT_14 ? [0x80, 0x02, 0x01, 0x02] : [];
	const body = [...head, ...fields, ...params];
	return new Uint8Array([0x03, 0, body.length, ...body]);
}

/** A SUBSCRIBE for `room/video` whose only parameter is an AUTHORIZATION TOKEN holding `token`. */
function subscribeWithToken(version: IetfVersion, token: readonly number[]): Uint8Array {
	return subscribeWith(version, tokenParams(version, token));
}

/** An update to that SUBSCRIBE whose only parameters are AUTHORIZATION TOKENs holding `tokens`. */
function updateWithTokens(version: IetfVersion, ...tokens: (readonly number[])[]): Uint8Array {
	// Draft-14 names its own Request ID and the subscription's, then the range, priority,
	// and forward inline. Drafts 15 and 16 name both IDs, and later drafts only its own,
	// which must be fresh: the subscription already spent 0.
	const head =
		version === Version.DRAFT_14
			? [2, 0, 0, 0, 0, 0x80, 0x01]
			: version === Version.DRAFT_15 || version === Version.DRAFT_16
				? [2, 0]
				: [2];
	const body = [...head, ...tokenParams(version, ...tokens)];
	return new Uint8Array([0x02, 0, body.length, ...body]);
}

/**
 * A request's AUTHORIZATION TOKEN decodes by the SETUP option's rules, so one this endpoint
 * cannot use closes the session with the draft's code. Draft-14 carries the request on the
 * control stream, a strict draft on its own stream.
 */
for (const [version, alpn] of [
	[Version.DRAFT_14, ALPN.DRAFT_14],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const) {
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
				maxRequestId: 100n,
				version,
				client: false,
			});
			try {
				const stream =
					version === Version.DRAFT_14
						? await Stream.accept(pair.client, version)
						: await Stream.open(pair.client, { version });
				if (!stream) throw new Error("no control stream");
				await stream.writer.write(subscribeWithToken(version, token));

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
for (const [version, alpn] of [
	[Version.DRAFT_14, ALPN.DRAFT_14],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const) {
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
			try {
				const stream =
					version === Version.DRAFT_14
						? await Stream.accept(pair.client, version)
						: await Stream.open(pair.client, { version });
				if (!stream) throw new Error("no control stream");
				await stream.writer.write(subscribeWith(version, [0]));
				// Draft-14's control stream also carries the PUBLISH_NAMESPACE for `room`.
				for (;;) {
					const type = await stream.reader.u53();
					if (type === SubscribeOk.id) break;
					await stream.reader.read(await stream.reader.u16());
				}
				await SubscribeOk.decode(stream.reader, version);
				await stream.writer.write(updateWithTokens(version, token));

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

/** Skip the messages ahead of the first `id`, such as draft-14's PUBLISH_NAMESPACE. */
async function skipTo(reader: Stream["reader"], id: number) {
	for (;;) {
		if ((await reader.u53()) === id) return;
		await reader.read(await reader.u16());
	}
}

/** A server publishing `room/video`, and the client's stream to send requests on. */
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
	// Through draft 16 every request rides the control stream.
	const control16 = version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16;
	const stream = control16 ? await Stream.accept(pair.client, version) : await Stream.open(pair.client, { version });
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
 * Section 8.9: a request whose tokens repeat is rejected MALFORMED_AUTH_TOKEN, which draft-15
 * moved from 0x10 to 0x4, and the session stays open for the next request.
 */
for (const [version, alpn, malformed] of [
	[Version.DRAFT_14, ALPN.DRAFT_14, 0x10],
	[Version.DRAFT_22, ALPN.DRAFT_22, 0x4],
] as const) {
	test(`a repeated request token rejects only the request ${alpn}`, async () => {
		const { pair, stream, close } = await serving(version, alpn);
		try {
			await stream.writer.write(subscribeWith(version, tokenParams(version, TOKEN, TOKEN)));
			if (version === Version.DRAFT_14) {
				await skipTo(stream.reader, SubscribeError.id);
				expect((await SubscribeError.decode(stream.reader, version)).errorCode).toBe(malformed);
			} else {
				expect(await stream.reader.u53()).toBe(RequestError.id);
				expect((await RequestError.decode(stream.reader, version)).errorCode).toBe(malformed);
			}

			const next = version === Version.DRAFT_14 ? stream : await Stream.open(pair.client, { version });
			await next.writer.write(subscribeWith(version, tokenParams(version, TOKEN, [0x03, 0x00, 0xbb]), 2));
			await skipTo(next.reader, SubscribeOk.id);
			await SubscribeOk.decode(next.reader, version);
		} finally {
			close();
		}
	});
}

/**
 * An update whose tokens repeat is answered MALFORMED_AUTH_TOKEN, not NOT_SUPPORTED, naming
 * the update's own Request ID on draft 16.
 */
for (const [version, alpn, requestId] of [
	[Version.DRAFT_16, ALPN.DRAFT_16, 2n],
	[Version.DRAFT_22, ALPN.DRAFT_22, undefined],
] as const) {
	test(`a repeated update token is malformed ${alpn}`, async () => {
		const { stream, close } = await serving(version, alpn);
		try {
			await stream.writer.write(subscribeWith(version, [0]));
			await skipTo(stream.reader, SubscribeOk.id);
			await SubscribeOk.decode(stream.reader, version);
			await stream.writer.write(updateWithTokens(version, TOKEN, TOKEN));
			await skipTo(stream.reader, RequestError.id);
			const error = await RequestError.decode(stream.reader, version);
			expect(error.errorCode).toBe(0x4);
			expect(error.requestId).toBe(requestId);
		} finally {
			close();
		}
	});
}
