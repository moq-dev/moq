/** Serve Rust-encoded range updates with the JS publisher and return its actual wire bytes. */
import assert from "node:assert/strict";
import { Publisher } from "../../js/net/src/ietf/publisher.ts";
import { RequestOk } from "../../js/net/src/ietf/request.ts";
import { NativeSession } from "../../js/net/src/ietf/session.ts";
import { Subscribe } from "../../js/net/src/ietf/subscribe.ts";
import type { IetfVersion } from "../../js/net/src/ietf/version.ts";
import { createMockTransportPair } from "../../js/net/src/mock.ts";
import { Producer as OriginProducer } from "../../js/net/src/origin.ts";
import * as Path from "../../js/net/src/path.ts";
import { Stream } from "../../js/net/src/stream.ts";

console.debug = console.error;
console.info = console.error;
const input: { draft: number; subscribe: number[]; updates: number[][] } = JSON.parse(process.argv[2]);
const version = (0xff000000 + input.draft) as IetfVersion;
const pair = createMockTransportPair(`moqt-${input.draft}`);
const origin = new OriginProducer();
const publication = origin.createBroadcast(Path.from("room"));
const track = publication.broadcast.createTrack("video");
publication.announce();
const publisher = new Publisher({
	quic: pair.server,
	session: new NativeSession(pair.server, version, false),
	publish: origin.consume(),
	requiresSolicitation: false,
});
const incoming = pair.client.incomingUnidirectionalStreams.getReader();
const client = await Stream.open(pair.client, { version });
const server = await Stream.accept(pair.server, version);
assert(server);
const responses: number[][] = [];
const streams: number[][] = [];
function publishGroup() {
	const group = track.appendGroup();
	for (let object = 0; object < 2; object++) group.writeFrame({ payload: new Uint8Array([group.sequence, object]) });
	group.finish();
}
async function response(expected: number) {
	const type = await client.reader.u53();
	assert.equal(type, expected, `draft-${input.draft}: range update was refused`);
	const size = await client.reader.u16();
	const body = await client.reader.read(size);
	responses.push([type, size >> 8, size & 0xff, ...body]);
}
async function stream() {
	const next = await incoming.read();
	assert(!next.done, "publisher ended without the owed group stream");
	const reader = next.value.getReader();
	const bytes: number[] = [];
	try {
		for (;;) {
			const chunk = await reader.read();
			if (chunk.done) break;
			bytes.push(...chunk.value);
		}
	} finally {
		reader.releaseLock();
	}
	streams.push(bytes);
}
let serving: Promise<void> | undefined;
try {
	if (input.draft >= 20) for (let i = 0; i < 4; i++) publishGroup();
	await client.writer.write(Uint8Array.from(input.subscribe));
	assert.equal(await server.reader.u53(), Subscribe.id);
	serving = publisher.runSubscribe(await Subscribe.decode(server.reader, version), server);
	await response(4); // SUBSCRIBE_OK
	if (input.draft >= 20) {
		await client.writer.write(Uint8Array.from(input.updates[0]));
		await response(RequestOk.id);
		publishGroup(); // Group 4 follows the filled snapshot, still on the original subscription.
		for (let i = 0; i < 3; i++) await stream();
	} else {
		await client.writer.write(Uint8Array.from(input.updates[0]));
		await response(RequestOk.id);
		for (let i = 0; i < 9; i++) publishGroup();
	}
	const narrowing = input.updates.at(-1);
	assert(narrowing);
	await client.writer.write(Uint8Array.from(narrowing));
	await response(RequestOk.id);
	// Draft-18 must replay both groups skipped below the old floor, rather than silently
	// joining only the newest cached group. Draft-20 and later also verify subsequent live delivery.
	if (input.draft >= 20) for (let i = 5; i < 9; i++) publishGroup();
	for (let i = 0; i < 2; i++) await stream();
} finally {
	client.close();
	await serving;
	incoming.releaseLock();
	publisher.close();
	origin.close();
	pair.client.close();
	pair.server.close();
}
process.stdout.write(JSON.stringify({ responses, streams }));
