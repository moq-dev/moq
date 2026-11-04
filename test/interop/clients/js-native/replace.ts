/** Replace a claimed track while its relay is still reading, then subscribe immediately. @module */
import assert from "node:assert/strict";
import { Milli } from "@moq/clock";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

const [publishUrl, subscribeUrl, prefix, transport] = process.argv.slice(2);
assert(
	publishUrl && subscribeUrl && prefix && ["websocket", "webtransport"].includes(transport),
	"replace.ts PUBLISH_URL SUBSCRIBE_URL PREFIX TRANSPORT",
);
if (transport === "webtransport") install();
console.debug = console.error;
console.info = console.error;

function endpoint(address: string): URL {
	const url = new URL(address);
	if (transport === "websocket") url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
	return url;
}

const published = new Moq.Origin.Producer();
const received = [new Moq.Origin.Producer(), new Moq.Origin.Producer(), new Moq.Origin.Producer()];
const claim = published.requests(Moq.Path.from(prefix));
const broadcast = new Moq.Broadcast.Producer();
const path = Moq.Path.from(`${prefix}/entity`);
let track = broadcast.createTrack("value", { maxAge: null });
const serving = (async () => {
	for await (const request of claim.requested()) request.serve(broadcast);
})();

function write(sequence: bigint): void {
	const group = track.createGroup(sequence);
	group.writeFrame({ payload: new TextEncoder().encode(String(sequence)) });
	group.finish();
}

const options = { versions: ["moq-lite-07-wip"], websocket: { enabled: transport === "websocket" } };
const publisher = await Moq.Connection.connect({ ...options, url: endpoint(publishUrl), publish: published.consume() });
const readers = await Promise.all(
	received.map((origin) => Moq.Connection.connect({ ...options, url: endpoint(subscribeUrl), consume: origin })),
);
const following = received.map((origin) => origin.follow(path));
let ordered: Moq.Track.Ordered | undefined;
try {
	assert.equal(publisher.transport, transport);
	for (const reader of readers) assert.equal(reader.transport, transport);
	const active = await Promise.all(
		following.map(async (request) => {
			let active = request.active.peek();
			while (!active) {
				const closed = request.closed.peek();
				if (closed !== undefined) throw closed ?? new Error("claim closed without a broadcast");
				await Moq.Signals.race([request.active.changed(), request.closed]);
				active = request.active.peek();
			}
			return active;
		}),
	);
	const subscribe = (index: number, sequence: bigint) =>
		active[index]
			.track("value")
			.subscribe({ delay: { max: null }, order: "ascending", ranges: [{ start: { absolute: sequence } }] })
			.ordered();
	const check = (frame: Awaited<ReturnType<Moq.Track.Ordered["readFrame"]>>, sequence: bigint) => {
		assert(frame, "track ended before its replacement");
		assert.equal(frame.group, sequence);
		assert.equal(new TextDecoder().decode(frame.payload), String(sequence));
	};
	write(0n);
	ordered = subscribe(0, 0n);
	check(await ordered.readFrame(), 0n);
	// Leave the old-group miss to the relay, as a failed live source does. This loop checks
	// only that each reader reaches the replacement; the explicit FETCHes below assert the codec.
	for (let sequence = 1n; sequence <= 64n; sequence++) {
		const continued = ordered.readFrame();
		track.abort(new Moq.Error.Stream(Moq.StreamCode.Internal, { message: "source failed" }));
		track = broadcast.createTrack("value", { maxAge: null });
		write(sequence);
		check(await continued, sequence);
		ordered.close();
		ordered = subscribe(0, sequence);
		check(await ordered.readFrame(), sequence);
	}
	for (const [index, sequence] of [66n, 68n].entries()) {
		const continued: ReturnType<Moq.Track.Ordered["readFrame"]> = ordered.readFrame();
		track.abort(new Moq.Error.Stream(Moq.StreamCode.Internal, { message: "source failed" }));
		track = broadcast.createTrack("value", { maxAge: null });
		const requests = track.requests();
		write(sequence);
		const next: Moq.Track.Ordered = subscribe(index + 1, sequence);
		try {
			// Explicitly fetch the next group so this always crosses FETCH in both directions.
			// The live subscription may also ask for the old group's tail, but its fill alone
			// could be served by SUBSCRIBE, hiding a broken FETCH decoder.
			const future = sequence + 1n;
			const fetching: Promise<Moq.Group.Consumer> = active[index + 1]
				.track("value")
				.fetchGroup(future, { maxDelay: Milli(Number.MAX_SAFE_INTEGER) });
			const filling = (async () => {
				for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
					try {
						if (fill.start < sequence) continue; // history the replacement no longer holds
						assert.equal(fill.start, future);
						assert.equal(fill.end, future + 1n);
						assert.equal(fill.maxDelay, null);
						const group = fill.accept(future);
						group.writeFrame({ payload: new TextEncoder().encode(String(future)) });
						group.finish();
						return;
					} finally {
						fill.close();
					}
				}
				throw new Error("replacement stopped serving its missing group");
			})();
			const [resumed, immediate, fetched] = await Promise.all([continued, next.readFrame(), fetching, filling]);
			check(resumed, sequence);
			check(immediate, sequence);
			try {
				assert.equal(fetched.sequence, future);
				assert.equal(await fetched.readString(), String(future));
			} finally {
				fetched.close();
			}
			check(await next.readFrame(), future);
		} catch (err) {
			next.close();
			throw err;
		} finally {
			requests.close();
		}
		ordered.close();
		ordered = next;
	}
	for (const request of following) assert.equal(request.closed.peek(), undefined);
	console.log("replacement automatic=64 immediate=2 resumed=2 missing-group-fetch=2");
} finally {
	ordered?.close();
	for (const request of following) request.close();
	track.close();
	claim.close();
	broadcast.close();
	published.close();
	for (const origin of received) origin.close();
	await serving;
	await Promise.all([publisher.close(), ...readers.map((reader) => reader.close())]);
}
