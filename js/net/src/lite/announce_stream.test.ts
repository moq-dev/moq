import { expect, test } from "bun:test";
import { createMockTransportPair } from "../mock.ts";
import { AnnounceStream } from "./announce_stream.ts";
import { Version } from "./version.ts";

const version = Version.DRAFT_07;

// Finishing right behind a send, before that send opened the stream, still finishes the stream
// the send opened, so the peer sees the FIN.
test("finishing behind an unwritten send finishes the stream it opens", async () => {
	const { client, server } = createMockTransportPair();
	try {
		const stream = AnnounceStream.of(client, version);
		const sent = stream.send({ status: "ok" });
		const finished = stream.finish();
		const incoming = server.incomingUnidirectionalStreams.getReader();
		const { value } = await incoming.read();
		if (!value) throw new Error("no stream");
		const reader = value.getReader();
		for (;;) {
			const { done } = await reader.read();
			if (done) break;
		}
		await sent;
		await finished;
	} finally {
		client.close();
		server.close();
	}
});

// A send that fails loses what this endpoint said: `broken` reports it, and a stream that never
// failed never settles it.
test("a failed send breaks the stream", async () => {
	const { client, server } = createMockTransportPair();
	try {
		const stream = AnnounceStream.of(client, version);
		let broken = false;
		void stream.broken.then(() => {
			broken = true;
		});
		await stream.send({ status: "ok" });
		await Promise.resolve();
		expect(broken).toBe(false);

		client.close();
		await expect(stream.send({ status: "ok" })).rejects.toThrow();
		await stream.broken;
		expect(broken).toBe(true);
	} finally {
		server.close();
	}
});
