import { expect, spyOn, test } from "bun:test";
import { exchangeSetup } from "../connection/handshake.ts";
import { SessionCode } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Stream, Writer } from "../stream.ts";
import { Connection } from "./connection.ts";
import { Group } from "./object.ts";
import { SetupOption, SetupOptions } from "./parameters.ts";
import { Setup } from "./setup.ts";
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
		connection.close();
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
			connection.close();
		}
	}
});

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

	const { control, early, solicit, hidden, cluster } = await exchangeSetup(pair.server, version, "test");
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
		connection.close();
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

	await expect(exchangeSetup(pair.server, version, "test")).rejects.toThrow("leading-ones varint");
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

	const { early } = await exchangeSetup(pair.server, version, "test");
	expect(early.length).toBe(0);
});
