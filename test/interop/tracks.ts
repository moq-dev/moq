// moq-lite-07 TRACKS both ways. `answer`: the JS publisher answers a Rust-encoded TRACKS request
// with its listing, through a removal and the broadcast's end. `echo`: JS decodes Rust-encoded
// listing events and encodes them back.
import { Milli } from "@moq/clock";
import { randomHop } from "../../js/net/src/hop.ts";
import { Publisher } from "../../js/net/src/lite/publisher.ts";
import { decodeTracksEventMaybe, encodeTracksEvent, Tracks } from "../../js/net/src/lite/tracks.ts";
import { Version } from "../../js/net/src/lite/version.ts";
import { createMockTransportPair } from "../../js/net/src/mock.ts";
import { Producer as OriginProducer } from "../../js/net/src/origin.ts";
import * as Path from "../../js/net/src/path.ts";
import { Reader, Stream, Writer } from "../../js/net/src/stream.ts";
import { Timescale } from "../../js/net/src/time.ts";

// Stdout is the byte channel back to Rust.
console.debug = console.error;
console.info = console.error;
const input: { mode: "answer" | "echo"; bytes: number[] } = JSON.parse(process.argv[2]);
const version = Version.DRAFT_07;
const bytes = Uint8Array.from(input.bytes);
const written: number[] = [];
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

if (input.mode === "answer") {
	const origin = new OriginProducer();
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("a", { timescale: Timescale.MILLI, maxAge: Milli(3000), priority: 9, refresh: 4 });
	broadcast.createTrack("b");

	const publisher = new Publisher(createMockTransportPair().server, version, randomHop(), origin.consume());
	const stream = new Stream({
		// The requester holds its side open while it lists: a FIN would cancel the listing.
		readable: new ReadableStream<Uint8Array>({ start: (controller) => controller.enqueue(bytes) }),
		writable: new WritableStream<Uint8Array>({ write: (chunk) => void written.push(...chunk) }),
		version,
	});
	const msg = await Tracks.decode(stream.reader, version);
	if (msg.broadcast !== Path.from("room")) throw new Error(`unexpected broadcast: ${msg.broadcast}`);
	const listing = publisher.runTracks(msg, stream);
	// Past SYNCED before the removal, so the removal is not folded into the initial set.
	await settle();
	broadcast.removeTrack("a");
	await settle();
	broadcast.close();
	await listing;
	origin.close();
} else {
	const reader = new Reader(undefined, bytes, version);
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(...chunk) }),
		version,
	);
	for (;;) {
		const event = await decodeTracksEventMaybe(reader, version);
		if (!event) break;
		await encodeTracksEvent(writer, event, version);
	}
	writer.close();
	await writer.closed;
}

process.stdout.write(JSON.stringify(written));
