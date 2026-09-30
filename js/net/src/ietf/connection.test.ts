import { expect, spyOn, test } from "bun:test";
import { StreamCode, Stream as StreamError } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Stream, Writer } from "../stream.ts";
import { Connection } from "./connection.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const PADDING = 0x132b3e28;

/** How long a session gets to react before we call it unmoved. */
const WAIT = 500;

/** A server-side session over `version`, and a client uni stream that sends only `type`. */
async function openUni(
	version: IetfVersion,
	alpn: string,
	type: number,
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
	await writer.u53(type);

	return { pair, connection, writer };
}

function timeout(message: string): Promise<never> {
	return new Promise((_resolve, reject) => setTimeout(() => reject(new Error(message)), WAIT));
}

/** The draft lets the receiver cancel padding at any time without affecting the session. */
test("a padding stream is cancelled", async () => {
	const { pair, connection, writer } = await openUni(Version.DRAFT_19, ALPN.DRAFT_19, PADDING);
	let closed = false;
	void pair.server.closed.then(() => {
		closed = true;
	});

	try {
		const err = await Promise.race([
			writer.closed.then(
				() => undefined,
				(err: unknown) => err,
			),
			timeout("not stopped"),
		]);
		expect(err).toBeInstanceOf(StreamError);
		expect((err as StreamError).code).toBe(StreamCode.Cancel);

		// A session close would come from the same handler that stopped the stream.
		await Bun.sleep(0);
		expect(closed).toBe(false);
	} finally {
		connection.close();
	}
});

/** An unknown stream type MUST close the session, including padding before draft-18 defined it. */
test("an unknown uni stream type closes the session", async () => {
	for (const [version, alpn, type] of [
		[Version.DRAFT_19, ALPN.DRAFT_19, 0],
		[Version.DRAFT_17, ALPN.DRAFT_17, PADDING],
	] as const) {
		const logged = spyOn(console, "error").mockImplementation(() => void 0);
		const { pair, connection } = await openUni(version, alpn, type);

		try {
			await Promise.race([pair.server.closed, timeout("session stayed up")]);
		} finally {
			logged.mockRestore();
			connection.close();
		}
	}
});
