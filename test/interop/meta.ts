// moq-lite-07 metadata subscriptions both ways. `serve`: the JS publisher serves a Rust-encoded
// metadata SUBSCRIBE and returns its Group Stream. `read`: JS reads a Rust publisher's metadata
// Group Stream, returning each frame's plain size and timestamp and the group's End.
import { randomHop } from "../../js/net/src/hop.ts";
import { Group } from "../../js/net/src/lite/group.ts";
import { Publisher } from "../../js/net/src/lite/publisher.ts";
import { Subscribe } from "../../js/net/src/lite/subscribe.ts";
import { Version } from "../../js/net/src/lite/version.ts";
import { createMockTransportPair, textFrame } from "../../js/net/src/mock.ts";
import { Producer as OriginProducer } from "../../js/net/src/origin.ts";
import * as Path from "../../js/net/src/path.ts";
import { Reader, Stream } from "../../js/net/src/stream.ts";
import { Timescale, Timestamp } from "../../js/net/src/time.ts";

// Stdout is the byte channel back to Rust.
console.debug = console.error;
console.info = console.error;
const input: { mode: "serve" | "read"; bytes: number[]; compression: "deflate" | null } = JSON.parse(process.argv[2]);
const version = Version.DRAFT_07;
const bytes = Uint8Array.from(input.bytes);

async function drain(readable: ReadableStream<Uint8Array>): Promise<number[]> {
	const out: number[] = [];
	for await (const chunk of readable) out.push(...chunk);
	return out;
}

if (input.mode === "serve") {
	const pair = createMockTransportPair();
	const origin = new OriginProducer();
	const broadcast = origin.route(Path.from("room")).broadcast;
	const track = broadcast.createTrack("video", {
		timescale: Timescale.MILLI,
		maxAge: null,
		compression: input.compression ?? undefined,
	});
	if (!track) throw new Error("the track already exists");

	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	// The subscriber holds its side of the Subscribe Stream open, as a live subscription does.
	const client = await Stream.open(pair.client, { version });
	const incoming = pair.server.incomingBidirectionalStreams.getReader();
	const accepted = await incoming.read();
	if (accepted.done) throw new Error("the publisher never accepted the subscribe stream");
	const stream = new Stream({ readable: accepted.value.readable, writable: accepted.value.writable, version });
	const msg = await Subscribe.decode(new Reader(undefined, bytes, version), version);
	if (!msg.meta) throw new Error("not a metadata SUBSCRIBE");
	void publisher.runSubscribe(msg, stream);
	const group = track.createGroup(0n);
	group.writeFrame(textFrame("a".repeat(300), Timestamp.fromMillis(2)));
	group.writeFrame(textFrame("", Timestamp.fromMillis(3)));
	group.writeFrame(textFrame("hello", Timestamp.fromMillis(4)));
	group.finishAt(5n);
	const opened = pair.client.incomingUnidirectionalStreams.getReader();
	const next = await opened.read();
	if (next.done) throw new Error("the publisher opened no group stream");
	const served = await drain(next.value);
	client.close();
	publisher.close();
	origin.close();
	process.stdout.write(JSON.stringify(served));
	process.exit(0);
} else {
	const reader = new Reader(undefined, bytes, version);
	await reader.u53(); // stream type
	const header = await Group.decode(reader, version);
	const sizes: number[] = [];
	const times: string[] = [];
	let ts = header.key;
	let highest: bigint | undefined;
	let end: string | undefined;
	while (!(await reader.done())) {
		// A metadata FRAME: the plain size plus one, then the timestamp delta, and no payload.
		const length = await reader.u53();
		if (length === 0) {
			end = ((highest ?? header.key) + (await reader.u62())).toString();
			continue;
		}
		const zigzag = await reader.u62();
		ts += (zigzag >> 1n) ^ -(zigzag & 1n);
		if (highest === undefined || ts > highest) highest = ts;
		times.push(ts.toString());
		sizes.push(length - 1);
	}
	process.stdout.write(JSON.stringify({ key: header.key.toString(), sizes, times, end }));
}
