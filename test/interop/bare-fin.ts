/** Exchange subscription response bytes with Rust using in-memory transports. */
import assert from "node:assert/strict";
import { ProtocolViolation } from "../../js/net/src/error.ts";
import { randomHop } from "../../js/net/src/hop.ts";
import { NativeSession } from "../../js/net/src/ietf/adapter.ts";
import { PublishDone } from "../../js/net/src/ietf/publish.ts";
import { Subscribe as IetfSubscribe, SubscribeOk } from "../../js/net/src/ietf/subscribe.ts";
import { Subscriber as IetfSubscriber } from "../../js/net/src/ietf/subscriber.ts";
import { Version as IetfVersion } from "../../js/net/src/ietf/version.ts";
import { StreamId } from "../../js/net/src/lite/stream.ts";
import { encodeSubscribeResponse, Subscribe, SubscribeEnd, SubscribeStart } from "../../js/net/src/lite/subscribe.ts";
import { Subscriber as LiteSubscriber } from "../../js/net/src/lite/subscriber.ts";
import { Track, TrackInfo } from "../../js/net/src/lite/track.ts";
import { Version as LiteVersion } from "../../js/net/src/lite/version.ts";
import { createMockTransportPair } from "../../js/net/src/mock.ts";
import * as Path from "../../js/net/src/path.ts";
import { Stream, Writer } from "../../js/net/src/stream.ts";
import type { Subscriber as TrackSubscriber } from "../../js/net/src/track.ts";

// Stdout is the response-byte channel back to Rust.
console.debug = console.error;
const input: { version: string; started: boolean; clean: boolean; responses: number[] } = JSON.parse(process.argv[2]);
const pair = createMockTransportPair(input.version);
const bytes: number[] = [];
const output = new Writer(
	new WritableStream<Uint8Array>({
		write: (chunk) => {
			bytes.push(...chunk);
		},
	}),
);
const path = Path.from("room");
const lite: Record<string, LiteVersion> = {
	"moq-lite-05": LiteVersion.DRAFT_05,
	"moq-lite-06": LiteVersion.DRAFT_06,
	"moq-lite-07-wip": LiteVersion.DRAFT_07,
};
const version = lite[input.version];
let reader: TrackSubscriber;
let peer: Stream | undefined;
if (version !== undefined) {
	const subscriber = new LiteSubscriber(pair.client, version, randomHop());
	reader = subscriber.consume(path).track("video").subscribe();
	const info = await Stream.accept(pair.server);
	assert(info);
	assert.equal(await info.reader.u53(), StreamId.Track);
	await Track.decode(info.reader, version);
	await new TrackInfo({}).encode(info.writer, version);
	info.close();
	peer = await Stream.accept(pair.server);
	assert(peer);
	assert.equal(await peer.reader.u53(), StreamId.Subscribe);
	await Subscribe.decode(peer.reader, version);
	if (input.started) await encodeSubscribeResponse(output, { start: new SubscribeStart(0) }, version);
	if (input.clean) await encodeSubscribeResponse(output, { end: new SubscribeEnd(0) }, version);
} else {
	assert.equal(input.version, "moqt-19");
	const version = IetfVersion.DRAFT_19;
	const subscriber = new IetfSubscriber({ session: new NativeSession(pair.client, version, true) });
	reader = subscriber.consume(path).track("video").subscribe();
	peer = await Stream.accept(pair.server, version);
	assert(peer);
	assert.equal(await peer.reader.u53(), IetfSubscribe.id);
	const request = await IetfSubscribe.decode(peer.reader, version);
	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({ requestId: request.requestId, trackAlias: 0n }).encode(peer.writer, version);
	if (input.clean) {
		await output.u53(PublishDone.id);
		await new PublishDone({ statusCode: 0x2, streamCount: 0n, reasonPhrase: "done" }).encode(output, version);
	}
}
// These bytes came from Rust's encoder; only the omitted end message is malformed.
if (input.responses.length) await peer.writer.write(Uint8Array.from(input.responses));
await peer.writer.close();
const closed = await reader.closed;
if (input.clean) {
	assert.equal(closed, null);
	assert.equal(await reader.recvGroup(), undefined);
} else {
	assert(closed instanceof ProtocolViolation);
	await assert.rejects(reader.recvGroup(), ProtocolViolation);
}
// Rust feeds the JS-encoded bytes into its own subscriber, followed by FIN.
console.log(JSON.stringify(bytes));
