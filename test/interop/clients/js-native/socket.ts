/** Socket convention interop over an authenticated relay, with a harness-owned drain acknowledgment. */
import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

install();
console.debug = console.error;
console.info = console.error;
const [role, address, prefix, version] = process.argv.slice(2);
assert(address && prefix !== undefined && (role === "dial" || role === "accept") && version);
const origin = new Moq.Origin.Producer();
const endpoint = new Moq.Socket.Endpoint(origin, Moq.Path.from(prefix), role === "dial" ? "dialer" : "acceptor");
const next = role === "accept" ? endpoint.acceptTrack() : undefined;
const connection = await Moq.Connection.connect({
	url: new URL(address),
	versions: [version],
	websocket: { enabled: false },
	publish: origin.consume(),
	consume: origin,
});
const lines = createInterface({ input: process.stdin });
const commands = lines[Symbol.asyncIterator]();
// Every Incoming is a claim: a pipelined request can outlive its refused metadata.
// Verify each new incoming once; a failed caller's dial still fails the harness.
async function accepted(): Promise<Moq.Socket.TrackStream> {
	for (;;) {
		const incoming = await endpoint.acceptTrack();
		try {
			return await incoming.accept();
		} catch {
			/* The claimed original dial was canceled or no longer answers. */
		}
	}
}

try {
	if (role === "accept") process.stdout.write("socket ready\n");
	// Refuse before opening data sessions, so the probe is independent of late
	// unverified queries for a completed half.
	if (next) {
		(await next).reject();
	} else {
		await assert.rejects(endpoint.dialTrack("acceptor"));
	}
	const stream = next ? await accepted() : await endpoint.dialTrack("acceptor");
	assert.equal(stream.peer, role === "dial" ? "acceptor" : "dialer");
	assert.match(stream.session, /^[0-9a-f]{32}$/);
	const own = role === "dial" ? 16 : 32;
	const peer = role === "dial" ? 32 : 16;
	// Exchange each independent message as a request and reply.
	const reader = stream.consumer.ordered();
	for (let i = 0; i < 3; i++) {
		if (role === "dial") stream.producer.writeFrame({ payload: new Uint8Array([own + i]) });
		const frame = await reader.readFrame();
		assert.equal(frame?.group, BigInt(i));
		assert.deepEqual(frame?.payload, new Uint8Array([peer + i]));
		if (role === "accept") stream.producer.writeFrame({ payload: new Uint8Array([own + i]) });
	}
	stream.producer.finish();
	assert.equal(await reader.readFrame(), undefined);
	const second = next ? await accepted() : await endpoint.dialTrack("acceptor");
	const resetReader = second.consumer.ordered();
	if (role === "dial") {
		second.producer.writeFrame({ payload: new Uint8Array([48]) });
		await assert.rejects(resetReader.readFrame());
	} else {
		assert.deepEqual((await resetReader.readFrame())?.payload, new Uint8Array([48]));
		second.producer.abort(new Moq.Error.Stream(Moq.StreamCode(70)));
	}
	console.error(`socket verified ${role}`);
	if (role === "accept") assert.equal((await commands.next()).value, "socket drained");
} finally {
	lines.close();
	endpoint.close();
	try {
		await connection.close();
	} finally {
		origin.close();
	}
}
