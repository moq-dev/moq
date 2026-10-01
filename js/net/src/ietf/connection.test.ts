import { expect, spyOn, test } from "bun:test";
import { SessionCode } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Stream, Writer } from "../stream.ts";
import { Connection } from "./connection.ts";
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
