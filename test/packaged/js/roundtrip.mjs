// Minimal connection and media round trip for the packaged @moq/net.
//
// Publishes one frame to a relay and subscribes to it back through the same
// relay, so the bytes cross the wire in both directions. Runs from the isolated
// consumer, so the only @moq code involved is what the staged archives shipped.
//
// Run under bun, not node: @moq/web-transport ships TypeScript sources, and
// node refuses to strip types inside node_modules.
//
//     bun roundtrip.mjs --url http://127.0.0.1:4470
import { parseArgs } from "node:util";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

// Node has no native WebTransport. `install()` puts moq's prebuilt QUIC/HTTP3
// addon on globalThis, which Moq.Connection.connect reads at call time.
install();

const { values } = parseArgs({
	options: {
		url: { type: "string" },
		timeout: { type: "string", default: "20" },
	},
});

const timeoutMs = Number.parseFloat(values.timeout) * 1000;
if (!values.url || !Number.isFinite(timeoutMs) || timeoutMs <= 0) {
	console.error("usage: roundtrip.mjs --url URL [--timeout S>0]");
	process.exit(2);
}

const PATH = "packaged-consumer";
const TRACK = "probe";
const PAYLOAD = "packaged-consumer round trip";

async function run() {
	const publisher = await Moq.Connection.connect(new URL(values.url));
	const subscriber = await Moq.Connection.connect(new URL(values.url));
	const broadcast = new Moq.Broadcast.Producer();
	try {
		// The track stays open until the frame has been read back. Closing a track
		// nobody has subscribed to yet leaves the relay nothing to serve, and the
		// subscribe below times out instead of failing.
		const track = broadcast.createTrack(TRACK);
		const group = track.appendGroup();
		group.writeString(PAYLOAD);
		group.close();

		publisher.publish(Moq.Path.from(PATH), broadcast);

		// Subscribing before the relay has seen the announce resets the stream, so
		// wait for the announcement first. The timeout below bounds the wait.
		const announced = subscriber.announced(Moq.Path.from(PATH));
		try {
			for (;;) {
				const entry = await announced.next();
				if (!entry) throw new Error("connection closed before the broadcast was announced");
				if (entry.active) break;
			}
		} finally {
			announced.close();
		}

		const consumer = subscriber.consume(Moq.Path.from(PATH)).subscribe(TRACK, { priority: 0 });
		const received = await consumer.recvGroup();
		if (!received) throw new Error("the track closed before delivering a group");
		const frame = await received.readFrame();
		if (!frame) throw new Error("the group closed before delivering a frame");

		const text = new TextDecoder().decode(frame.payload);
		if (text !== PAYLOAD) throw new Error(`round tripped ${JSON.stringify(text)}`);
		console.log(`  ok   round tripped ${frame.payload.byteLength} bytes through ${values.url}`);
	} finally {
		broadcast.close();
		publisher.close();
		subscriber.close();
	}
}

const timeout = new Promise((_, reject) => setTimeout(() => reject(new Error("timed out")), timeoutMs).unref?.());

try {
	await Promise.race([run(), timeout]);
	process.exit(0);
} catch (err) {
	console.error(`  FAIL round trip: ${err instanceof Error ? err.message : String(err)}`);
	process.exit(1);
}
