/**
 * The JavaScript peer of `just test datagram-tracks`: publishes or reads broadcast `dg` over
 * QMux on WebSocket, which carries datagrams in order, so a delivery the test misses is a bug.
 *
 * `dg` holds a timed datagram track `audio`, an untimed datagram track `events`, and a
 * stream track also named `audio`, which a datagram track never shares a namespace with.
 *
 *     bun client.ts URL ALPN publish     # prints `ready`, then reads `send` and `finish` lines
 *     bun client.ts URL ALPN subscribe   # prints `subscribed`, then `received` once it read the data
 *
 * The Rust test (rs/moq-tokio/tests/integration/datagram_tracks.rs) drives either end and
 * carries the same expectations.
 *
 * @module
 */
import assert from "node:assert/strict";
import { Session } from "@moq/qmux";
import * as Moq from "../../js/net/src/index.ts";

// stdout carries the handshake to the Rust test, so library debug logs go to stderr.
console.debug = console.error;

const [address, protocol, mode] = process.argv.slice(2);
if (!address || !protocol || !["publish", "subscribe"].includes(mode))
	throw new Error("expected URL ALPN publish|subscribe");

// The subscriber's priority on `audio`, which the publisher waits to see before writing.
const PRIORITY = 17;
// Past 2^53, so a lossy number conversion anywhere on the path shows.
const AUDIO = [
	{ sequence: 1n, micros: 1_234_567, payload: [1, 2, 3] },
	{ sequence: (1n << 53n) + 1n, micros: 2_000_000, payload: [4, 5] },
];
const EVENTS = [
	{ sequence: 0n, payload: [...new TextEncoder().encode("hello")] },
	{ sequence: 7n, payload: [] },
];
const FRAME = { millis: 5, payload: [9] };

const origin = new Moq.Origin.Producer();
const url = new URL(address);
const transport = new Session(url, { protocols: [protocol], versions: { [protocol]: null }, requireProtocol: true });
await transport.ready;
const session = await Moq.Connection.connect({
	url,
	transport,
	publish: mode === "publish" ? origin.consume() : undefined,
	consume: mode === "subscribe" ? origin : undefined,
});

const lines = (async function* () {
	let buffered = "";
	for await (const chunk of Bun.stdin.stream().pipeThrough(new TextDecoderStream())) {
		buffered += chunk;
		for (let end = buffered.indexOf("\n"); end >= 0; end = buffered.indexOf("\n")) {
			yield buffered.slice(0, end);
			buffered = buffered.slice(end + 1);
		}
	}
})();

/** `count` datagrams from `track`, sorted by sequence. */
async function recvAll(track: Moq.Datagram.Consumer, count: number): Promise<Moq.Datagram.Datagram[]> {
	const got = [];
	while (got.length < count) {
		const datagram = await track.recv();
		assert(datagram, `${track.name} ended early`);
		got.push(datagram);
	}
	return got.sort((a, b) => (a.sequence < b.sequence ? -1 : 1));
}

async function expectLine(want: string) {
	const { value } = await lines.next();
	assert.equal(value, want, `expected ${want} from the test`);
}

try {
	if (mode === "publish") {
		const route = origin.route(Moq.Path.from("dg"), { hidden: true });
		const audio = route.broadcast.createDatagrams("audio", { timescale: Moq.Time.Timescale.MICRO, priority: -7 });
		const events = route.broadcast.createDatagrams("events");
		const track = route.broadcast.createTrack("audio", { timescale: Moq.Time.Timescale.MILLI });
		route.announce();
		console.log("ready");

		await expectLine("send");
		while (audio.priority.peek() !== PRIORITY) await audio.priority.changed();
		for (const { sequence, micros, payload } of AUDIO)
			audio.insert({
				sequence,
				timestamp: Moq.Time.Timestamp.fromMicros(micros),
				payload: new Uint8Array(payload),
			});
		for (const { sequence, payload } of EVENTS)
			events.insert({
				sequence,
				timestamp: Moq.Time.Timestamp.unscaled(sequence),
				payload: new Uint8Array(payload),
			});
		track.writeFrame({
			timestamp: Moq.Time.Timestamp.fromMillis(FRAME.millis),
			payload: new Uint8Array(FRAME.payload),
		});

		await expectLine("finish");
		audio.finish();
		events.finish();
		track.finish();
		// The test closes stdin once the subscriber has read the end.
		assert.equal((await lines.next()).done, true, "unexpected line from the test");
	} else {
		const announcements = origin.consume().announced();
		// `live` can come first while a relay has yet to learn the publisher's route.
		for (;;) {
			const event = await announcements.next();
			assert(event, "announcements closed");
			if (event.kind === "present" && event.prefix === "dg") break;
		}
		announcements.close();
		const request = origin.consume().request(Moq.Path.from("dg"));
		let broadcast = request.active.peek();
		while (!broadcast) broadcast = await request.active.changed();

		const audio = broadcast.datagrams("audio");
		audio.setPriority(PRIORITY);
		const events = broadcast.datagrams("events");
		const track = broadcast.track("audio").subscribe();
		const audioInfo = await audio.info();
		assert.equal(audioInfo.timescale, Moq.Time.Timescale.MICRO, "audio timescale");
		assert.equal(audioInfo.priority, -7, "audio publisher priority");
		const eventsInfo = await events.info();
		assert.equal(eventsInfo.timescale, null, "events are untimed");
		assert.equal(eventsInfo.priority, 0, "events publisher priority");
		assert.equal((await track.info()).timescale, Moq.Time.Timescale.MILLI, "stream track timescale");
		// Answered, so what the publisher writes from now on reaches this session.
		console.log("subscribed");

		// Datagrams are unordered, so each track's arrive in any order.
		const audioGot = await recvAll(audio, AUDIO.length);
		for (const [i, want] of AUDIO.entries()) {
			const got = audioGot[i];
			assert.equal(got.sequence, want.sequence, "audio sequence");
			assert.equal(got.timestamp?.asMicros(), want.micros, "audio timestamp");
			assert.deepEqual([...got.payload], want.payload, "audio payload");
		}
		const eventsGot = await recvAll(events, EVENTS.length);
		for (const [i, want] of EVENTS.entries()) {
			const got = eventsGot[i];
			assert.equal(got.sequence, want.sequence, "events sequence");
			assert.equal(got.timestamp, undefined, "an untimed datagram has no timestamp");
			assert.deepEqual([...got.payload], want.payload, "events payload");
		}
		const group = await track.recvGroup();
		assert(group, "stream track ended early");
		const frame = await group.readFrame();
		assert(frame, "group ended early");
		assert.equal(frame.timestamp?.asMillis(), FRAME.millis, "frame timestamp");
		assert.deepEqual([...frame.payload], FRAME.payload, "frame payload");
		group.close();
		console.log("received");

		assert.equal(await audio.recv(), undefined, "audio ends");
		assert.equal(await events.recv(), undefined, "events end");
		assert.equal(await track.recvGroup(), undefined, "stream track ends");
		audio.close();
		events.close();
		track.close();
		request.close();
	}
} finally {
	await session.close();
	origin.close();
}
