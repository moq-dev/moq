// Receive Rust's announcement in JS, then forward the stamped route back to Rust.

import { HopSchema } from "../../js/net/src/hop.ts";
import { AnnounceOk, encodeAnnounceBroadcast } from "../../js/net/src/lite/announce.ts";
import { Subscriber } from "../../js/net/src/lite/subscriber.ts";
import { Version } from "../../js/net/src/lite/version.ts";
import * as Path from "../../js/net/src/path.ts";
import { Writer } from "../../js/net/src/stream.ts";

console.debug = console.error;
const input: number[] = JSON.parse(process.argv[2]);
const version = Version.DRAFT_07;
const relay = HopSchema.parse(31n);
const quic = {
	createBidirectionalStream: async () => ({
		readable: new ReadableStream<Uint8Array>({ start: (controller) => controller.enqueue(Uint8Array.from(input)) }),
		writable: new WritableStream<Uint8Array>(),
	}),
} as unknown as WebTransport;
const subscriber = new Subscriber(quic, version, relay);
const announced = subscriber.announced();
try {
	const received = await announced.next();
	if (!received || received.prefix !== Path.from("room")) throw new Error("missing Rust announcement");
	const output: number[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void output.push(...chunk) }),
		version,
	);
	await new AnnounceOk(relay, 0).encode(writer, version);
	await encodeAnnounceBroadcast(
		writer,
		{ status: "active", suffix: received.prefix, hops: received.route.hops },
		version,
	);
	writer.close();
	await writer.closed;
	process.stdout.write(JSON.stringify(output));
} finally {
	announced.close();
	subscriber.close();
}
