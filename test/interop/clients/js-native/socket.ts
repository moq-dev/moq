/** Socket convention interop over an authenticated relay, with a harness-owned drain acknowledgment. */
import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

install();
console.debug = console.error;
console.info = console.error;
const [role, address, prefix, protocol, version] = process.argv.slice(2);
assert(
	address &&
		prefix !== undefined &&
		(role === "dial" || role === "accept") &&
		(protocol === "tcp" || protocol === "track") &&
		version,
);
const origin = new Moq.Origin.Producer();
const endpoint = new Moq.socket.Endpoint(origin, Moq.Path.from(prefix), role === "dial" ? "dialer" : "acceptor");
const next = role === "accept" ? (protocol === "tcp" ? endpoint.acceptTcp() : endpoint.acceptTrack()) : undefined;
const connection = await Moq.Connection.connect({
	url: new URL(address),
	versions: [version],
	websocket: { enabled: false },
	publish: origin.consume(),
	consume: origin,
});
const lines = createInterface({ input: process.stdin });
const commands = lines[Symbol.asyncIterator]();
try {
	if (role === "accept") process.stdout.write("socket ready\n");
	const stream = next
		? await (await next).accept()
		: protocol === "tcp"
			? await endpoint.dialTcp("acceptor")
			: await endpoint.dialTrack("acceptor");
	assert.equal(stream.peer, role === "dial" ? "acceptor" : "dialer");
	assert.match(stream.session, /^[0-9a-f]{32}$/);
	const own = role === "dial" ? 16 : 32;
	const peer = role === "dial" ? 32 : 16;
	if (stream instanceof Moq.socket.TcpStream) {
		// Exchange a request and reply before FIN, then drain both independent halves.
		for (let i = 0; i < 3; i++) stream.producer.writeFrame({ payload: new Uint8Array([own + i]) });
		stream.producer.finish();
		for (let i = 0; i < 3; i++)
			assert.deepEqual((await stream.consumer.readFrame())?.payload, new Uint8Array([peer + i]));
		assert.equal(await stream.consumer.readFrame(), undefined);
	} else {
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
	}
	const second = next
		? await (await (protocol === "tcp" ? endpoint.acceptTcp() : endpoint.acceptTrack())).accept()
		: protocol === "tcp"
			? await endpoint.dialTcp("acceptor")
			: await endpoint.dialTrack("acceptor");
	const resetReader = second instanceof Moq.socket.TcpStream ? second.consumer : second.consumer.ordered();
	if (role === "dial") {
		second.producer.writeFrame({ payload: new Uint8Array([48]) });
		await assert.rejects(resetReader.readFrame());
	} else {
		assert.deepEqual((await resetReader.readFrame())?.payload, new Uint8Array([48]));
		second.producer.abort(new Moq.Error.Stream(Moq.StreamCode(70)));
	}
	if (role === "dial") {
		await assert.rejects(protocol === "tcp" ? endpoint.dialTcp("acceptor") : endpoint.dialTrack("acceptor"));
	} else {
		for (;;) {
			const incoming = await (protocol === "tcp" ? endpoint.acceptTcp() : endpoint.acceptTrack());
			assert.equal(incoming.peer, "dialer");
			incoming.reject();
			// Incoming names are unverified. A late FETCH for a reset/finished half can
			// ask for it again; refuse those known identities before the fresh test dial.
			if (incoming.session !== stream.session && incoming.session !== second.session) break;
		}
	}
	console.error(`socket verified ${protocol} ${role}`);
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
