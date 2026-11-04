/** Resolve on-demand JS claims through a real lite-07 relay, including reader-first startup. @module */
import assert from "node:assert/strict";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

const [publishUrl, subscribeUrl, prefix, transport] = process.argv.slice(2);
assert(
	publishUrl && subscribeUrl && prefix && ["websocket", "webtransport"].includes(transport),
	"claims.ts PUBLISH_URL SUBSCRIBE_URL PREFIX TRANSPORT",
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
const received = new Moq.Origin.Producer();
const claim = published.requests(Moq.Path.from(prefix));
const broadcasts = new Map<Moq.Path.Valid, Moq.Broadcast.Producer>();
const serving = (async () => {
	for await (const request of claim.requested()) {
		// Model an application looking up an entity before answering, without a timer.
		await Promise.resolve();
		let broadcast = broadcasts.get(request.path);
		if (!broadcast) {
			broadcast = new Moq.Broadcast.Producer();
			broadcast
				.createTrack("value", { maxAge: null })
				.writeFrame({ payload: new TextEncoder().encode(request.path) });
			broadcasts.set(request.path, broadcast);
		}
		request.serve(broadcast);
	}
})();

async function read(request: Moq.Origin.Requesting | Moq.Origin.Following): Promise<void> {
	let active = request.active.peek();
	while (!active) {
		const closed = request.closed.peek();
		if (closed !== undefined) throw closed ?? new Error("request closed without a broadcast");
		await Moq.Signals.race([request.active.changed(), request.closed]);
		active = request.active.peek();
	}
	const track = active.track("value").subscribe().ordered();
	try {
		assert.equal(await track.readString(), request.path, "wrong claim answer");
	} finally {
		track.close();
	}
}

const options = { versions: ["moq-lite-07-wip"], websocket: { enabled: transport === "websocket" } };
const reader = await Moq.Connection.connect({ ...options, url: endpoint(subscribeUrl), consume: received });
let publisher: Awaited<ReturnType<typeof Moq.Connection.connect>> | undefined;
const late = Moq.Path.from(`${prefix}/late`);
const absent = received.request(late);
const followed = received.follow(late);
try {
	assert.equal(reader.transport, transport);
	// The claim exists locally but has not reached the relay. Empty discovery is terminal
	// for a request; only the follower waits for this publisher to connect.
	const error = await absent.closed;
	assert(error instanceof Moq.Error.Stream);
	assert.equal(error.code, Moq.StreamCode.Unroutable);
	publisher = await Moq.Connection.connect({ ...options, url: endpoint(publishUrl), publish: published.consume() });
	assert.equal(publisher.transport, transport);
	await read(followed);
	assert.equal(absent.active.peek(), undefined);
	assert.equal(absent.closed.peek(), error);

	// The late follower proves the relay has the claim before these one-shot requests.
	// Each reader has a fresh origin, without a discovery interest or materialized broadcast.
	for (const method of ["request", "follow"] as const) {
		for (const suffix of ["", `/${method}`]) {
			const origin = new Moq.Origin.Producer();
			const connection = await Moq.Connection.connect({
				...options,
				url: endpoint(subscribeUrl),
				consume: origin,
			});
			const request = origin[method](Moq.Path.from(`${prefix}${suffix}`));
			try {
				assert.equal(connection.transport, transport);
				await read(request);
				assert.equal(request.closed.peek(), undefined);
			} finally {
				request.close();
				origin.close();
				await connection.close();
			}
		}
	}
	assert.deepEqual([...broadcasts.keys()].sort(), [prefix, `${prefix}/follow`, late, `${prefix}/request`].sort());
	console.log("claims resolved own-path=2 child-path=2 late-follow=1 absent=Unroutable");
} finally {
	absent.close();
	followed.close();
	claim.close();
	for (const broadcast of broadcasts.values()) broadcast.close();
	published.close();
	received.close();
	await serving;
	await Promise.all([reader.close(), publisher?.close()]);
}
