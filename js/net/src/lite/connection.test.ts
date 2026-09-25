import { expect, test } from "bun:test";
import { createMockTransportPair } from "../mock.ts";
import { Stream } from "../stream.ts";
import { wireOf } from "../wire.ts";
import { Connection } from "./connection.ts";
import { Goaway } from "./goaway.ts";
import { StreamId } from "./stream.ts";
import { ALPN_04, Version } from "./version.ts";

async function sendGoaway(server: WebTransport, uri: string): Promise<void> {
	const stream = await Stream.open(server);
	await stream.writer.u53(StreamId.Goaway);
	await new Goaway(uri).encode(stream.writer, Version.DRAFT_04);
	stream.writer.close();
}

test("a lite GOAWAY keeps the session open, and a second one closes it", async () => {
	const pair = createMockTransportPair(ALPN_04);
	const connection = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.client,
		version: Version.DRAFT_04,
	});

	let closed = false;
	void connection.closed.then(() => {
		closed = true;
	});

	try {
		await sendGoaway(pair.server, "");
		const drain = await wireOf(connection).goaway;
		expect(drain.uri).toBe("");

		await new Promise((resolve) => setTimeout(resolve, 20));
		expect(closed).toBe(false);

		await sendGoaway(pair.server, "https://other.example/");
		await new Promise((resolve) => setTimeout(resolve, 50));
		expect(closed).toBe(true);
	} finally {
		connection.close();
	}
});
