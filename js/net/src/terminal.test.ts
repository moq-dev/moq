import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { ProtocolViolation, SessionCode, StreamCode } from "./error.ts";
import { hooks } from "./internal.ts";
import { Connection } from "./lite/connection.ts";
import { StreamId } from "./lite/stream.ts";
import { ALPN_06, Version } from "./lite/version.ts";
import { createMockTransportPair, textFrame } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Stream } from "./stream.ts";
import { Timescale } from "./time.ts";
import { Producer as TrackProducer } from "./track.ts";
import { wireOf } from "./wire.ts";

for (const release of [false, true]) {
	test(`${release ? "releasing" : "closing"} a track preserves finished groups and aborts its unfinished tail`, async () => {
		const producer = new TrackProducer("video").accept({ timescale: Timescale.MILLI });
		const complete = producer.appendGroup();
		complete.writeFrame(textFrame("complete"));
		complete.finish();
		const tail = producer.appendGroup();
		tail.writeFrame(textFrame("prefix"));
		const finished = complete.consume();
		const unfinished = tail.consume();
		producer.finish();
		try {
			if (release) hooks.releaseTrack(producer);
			else producer.close();
			expect(await finished.readString()).toBe("complete");
			expect(await finished.readFrame()).toBeUndefined();
			expect(await unfinished.readString()).toBe("prefix");
			await expect(unfinished.readFrame()).rejects.toMatchObject({ code: StreamCode.Cancel });
		} finally {
			finished.close();
			unfinished.close();
			producer.close();
		}
	});
}

test("an unanswered track request is refused with the dropped producer's error", async () => {
	const broadcast = new BroadcastProducer();
	const requested = wireOf(broadcast).requested();
	const track = broadcast.track("absent").subscribe();
	await requested;
	const info = track.info();
	broadcast.close();
	try {
		await expect(info).rejects.toMatchObject({ code: StreamCode.Internal });
		expect(await track.closed).toMatchObject({ code: StreamCode.Internal });
	} finally {
		track.close();
	}
});

test("a lite protocol violation reaches the subscribed track and its open group", async () => {
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const publication = origin.createBroadcast(Path.from("room"));
	publication.announce();
	const producer = publication.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	producer.appendGroup().writeFrame(textFrame("prefix"));
	const url = new URL("https://example.com");
	const client = new Connection({ url, quic: pair.client, version: Version.DRAFT_06 });
	const server = new Connection({
		url,
		quic: pair.server,
		version: Version.DRAFT_06,
		publish: origin.consume(),
		client: false,
	});
	const broadcast = wireOf(client).consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();
	const group = await track.recvGroup();
	try {
		expect(await group?.readString()).toBe("prefix");
		const duplicate = await Stream.open(pair.server, { version: Version.DRAFT_06 });
		await duplicate.writer.u53(StreamId.Session);
		expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
		expect(await track.closed).toBeInstanceOf(ProtocolViolation);
		await expect(group?.readFrame()).rejects.toBeInstanceOf(ProtocolViolation);
	} finally {
		group?.close();
		track.close();
		broadcast.close();
		client.abort();
		server.abort();
		publication.broadcast.close();
		origin.close();
	}
});
