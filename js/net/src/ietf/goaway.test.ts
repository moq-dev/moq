import { expect, test } from "bun:test";
import { Milli, Nano, sleep } from "@moq/clock";
import { SessionCode, StreamCode, StreamError } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { wireOf } from "../wire.ts";
import { AuthMessage, AuthOk, IetfAuthWire } from "./auth.ts";
import { Connection } from "./connection.ts";
import { GoAway } from "./goaway.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { RequestOk } from "./request.ts";
import { NativeSession } from "./session.ts";
import { SubscribeNamespace, SubscribeNamespaceEntry } from "./subscribe_namespace.ts";
import { ALPN, Version } from "./version.ts";

// GOAWAY rides the setup stream, with the peer's deadline, and on draft-18 a Request ID. The
// session keeps serving afterwards so its groups in flight finish while the caller migrates.
for (const [version, alpn, requestId] of [
	[Version.DRAFT_18, ALPN.DRAFT_18, 0n],
	[Version.DRAFT_22, ALPN.DRAFT_22, undefined],
] as const) {
	test(`a ${alpn} GOAWAY surfaces its URI and deadline without closing the session`, async () => {
		const pair = createMockTransportPair(alpn);
		const control = await Stream.open(pair.client, { version });
		const peer = await Stream.accept(pair.server, version);
		if (!peer) throw new Error("no setup stream");

		const connection = new Connection({
			url: new URL("https://relay.example/"),
			quic: pair.client,
			control,
			version,
			client: true,
		});

		let closed = false;
		void connection.closed.then(() => {
			closed = true;
		});

		try {
			await peer.writer.u53(GoAway.id);
			await new GoAway({ newSessionUri: "", timeout: 5000n, requestId }).encode(peer.writer, version);

			const drain = await wireOf(connection).goaway;
			expect(drain.uri).toBe("");
			expect(drain.timeout).toBe(Milli(5000));

			await sleep(Nano.fromMilli(Milli(20)));
			expect(closed).toBe(false);
		} finally {
			connection.abort();
		}
	});
}

test("a server rejects a client GOAWAY that names a redirect", async () => {
	const version = Version.DRAFT_18;
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const control = await Stream.open(pair.client, { version });
	const peer = await Stream.accept(pair.server, version);
	if (!peer) throw new Error("no setup stream");

	const connection = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.client,
		control,
		version,
		client: false,
	});

	let closed = false;
	void connection.closed.then(() => {
		closed = true;
	});

	try {
		await peer.writer.u53(GoAway.id);
		await new GoAway({ newSessionUri: "https://other.example/", timeout: 0n }).encode(peer.writer, version);
		await sleep(Nano.fromMilli(Milli(50)));
		expect(closed).toBe(true);
	} finally {
		connection.abort();
	}
});

test("a zero GOAWAY timeout reads as no deadline", async () => {
	const msg = new GoAway({ newSessionUri: "https://relay.example/next", timeout: 0n });
	expect(msg.drain()).toEqual({ uri: "https://relay.example/next", timeout: undefined });
});

for (const [version, alpn] of [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const) {
	for (const end of ["FIN", "reset", "STOP_SENDING"] as const) {
		test(`${alpn} control stream ${end} is a protocol violation`, async () => {
			const pair = createMockTransportPair(alpn);
			const control = await Stream.open(pair.client, { version });
			const peer = await Stream.accept(pair.server, version);
			if (!peer) throw new Error("no control stream");
			const connection = new Connection({
				url: new URL("https://relay.example/"),
				quic: pair.client,
				control,
				version,
				client: true,
			});
			try {
				if (end === "reset") peer.writer.reset(new StreamError(StreamCode.Cancel));
				else if (end === "STOP_SENDING") peer.reader.stop(new StreamError(StreamCode.Cancel));
				else peer.writer.close();
				expect((await pair.server.closed).closeCode).toBe(SessionCode.ProtocolViolation);
			} finally {
				connection.abort();
			}
		});
	}
}

for (const [version, alpn] of [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const) {
	test(`${alpn} rejects queued duplicate GOAWAYs before exposing a drain`, async () => {
		const pair = createMockTransportPair(alpn);
		const control = await Stream.open(pair.client, { version });
		const peer = await Stream.accept(pair.server, version);
		if (!peer) throw new Error("no control stream");
		const connection = new Connection({
			url: new URL("https://relay.example/"),
			quic: pair.client,
			control,
			version,
			client: true,
		});
		// A caller may close immediately on a redirect, so the protocol error must win.
		void wireOf(connection).goaway.then(() => connection.abort());
		const frame = version === Version.DRAFT_18 ? [0x10, 0, 3, 0, 0, 0] : [0x10, 0, 2, 0, 0];
		try {
			await peer.writer.write(new Uint8Array([...frame, ...frame]));
			expect((await pair.server.closed).closeCode).toBe(SessionCode.ProtocolViolation);
			expect(wireOf(connection).goaway.peek()).toBeUndefined();
		} finally {
			connection.abort();
		}
	});
}

for (const [version, alpn] of [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const) {
	test(`${alpn} AUTH replies handle GOAWAY across grants`, async () => {
		const pair = createMockTransportPair(alpn);
		const wire = new IetfAuthWire(new NativeSession(pair.client, version, true));
		const stream = await wire.present(new Uint8Array());
		const peer = await Stream.accept(pair.server, version);
		if (!peer) throw new Error("no AUTH stream");
		try {
			await peer.writer.u53(GoAway.id);
			await new GoAway({ newSessionUri: "" }).encode(peer.writer, version);
			await new AuthOk(new Path.Patterns(), new Path.Patterns()).encode(peer.writer, version);
			expect(await wire.read(stream)).toEqual({
				grant: { publish: new Path.Patterns(), subscribe: new Path.Patterns(), expires: undefined },
			});
			await peer.writer.u53(GoAway.id);
			await new GoAway({ newSessionUri: "" }).encode(peer.writer, version);
			await expect(wire.read(stream)).rejects.toThrow("second GOAWAY");
		} finally {
			pair.client.close();
		}
	});

	test(`${alpn} an AUTH presenter can send GOAWAY before withdrawing`, async () => {
		const pair = createMockTransportPair(alpn);
		const wire = new IetfAuthWire(new NativeSession(pair.client, version, true));
		const stream = await Stream.open(pair.client, { version });
		const peer = await Stream.accept(pair.server, version);
		if (!peer) throw new Error("no AUTH stream");
		try {
			await new AuthMessage(1n, new Uint8Array()).encode(peer.writer, version);
			expect(await stream.reader.u53()).toBe(AuthMessage.id);
			await wire.accept(stream);
			const withdrawn = wire.withdrawn(stream);
			await peer.writer.u53(GoAway.id);
			await new GoAway({ newSessionUri: "" }).encode(peer.writer, version);
			peer.writer.close();
			await withdrawn;
		} finally {
			pair.client.close();
		}
	});
}

for (const [version, alpn] of [
	[Version.DRAFT_18, ALPN.DRAFT_18],
	[Version.DRAFT_22, ALPN.DRAFT_22],
] as const) {
	test(`${alpn} namespace discovery reads GOAWAY before its response and between entries`, async () => {
		const pair = createMockTransportPair(alpn);
		const control = await Stream.open(pair.client, { version });
		await Stream.accept(pair.server, version);
		const connection = new Connection({
			url: new URL("https://relay.example/"),
			quic: pair.client,
			control,
			version,
			client: true,
		});
		const announced = connection.announced();
		try {
			const request = await Stream.accept(pair.server, version);
			if (!request) throw new Error("no namespace subscription");
			expect(await request.reader.u53()).toBe(SubscribeNamespace.id);
			await SubscribeNamespace.decode(request.reader, version);
			await request.writer.u53(GoAway.id);
			await new GoAway({ newSessionUri: "" }).encode(request.writer, version);
			await request.writer.u53(RequestOk.id);
			await new RequestOk().encode(request.writer, version);
			await request.writer.u53(SubscribeNamespaceEntry.id);
			await new SubscribeNamespaceEntry({ suffix: Path.from("room") }).encode(request.writer, version);
			expect((await announced.next())?.prefix).toBe(Path.from("room"));
			await request.writer.write(Uint8Array.of(0x10, 0, 2, 0, 0));
			expect((await pair.server.closed).closeCode).toBe(SessionCode.ProtocolViolation);
		} finally {
			announced.close();
			connection.abort();
		}
	});

	test(`${alpn} an idle advertisement still rejects a second GOAWAY`, async () => {
		const pair = createMockTransportPair(alpn);
		const control = await Stream.open(pair.server, { version });
		await Stream.accept(pair.client, version);
		const origin = new OriginProducer();
		origin.createBroadcast(Path.from("room")).announce();
		const connection = new Connection({
			url: new URL("https://relay.example/"),
			quic: pair.server,
			control,
			version,
			client: false,
			publish: origin.consume(),
		});
		try {
			const request = await Stream.accept(pair.client, version);
			if (!request) throw new Error("no advertisement");
			expect(await request.reader.u53()).toBe(PublishNamespace.id);
			await PublishNamespace.decode(request.reader, version);
			await request.writer.u53(RequestOk.id);
			await new RequestOk().encode(request.writer, version);
			await request.writer.write(Uint8Array.of(0x10, 0, 2, 0, 0, 0x10, 0, 2, 0, 0));
			expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
		} finally {
			connection.abort();
			origin.close();
		}
	});

	for (const end of ["FIN", "reset"] as const) {
		test(`${alpn} an idle advertisement whose answer ends by ${end} is offered again`, async () => {
			const pair = createMockTransportPair(alpn);
			const control = await Stream.open(pair.server, { version });
			await Stream.accept(pair.client, version);
			const origin = new OriginProducer();
			origin.createBroadcast(Path.from("room")).announce();
			const connection = new Connection({
				url: new URL("https://relay.example/"),
				quic: pair.server,
				control,
				version,
				client: false,
				publish: origin.consume(),
			});
			const offered = async () => {
				const request = await Stream.accept(pair.client, version);
				if (!request) throw new Error("no advertisement");
				expect(await request.reader.u53()).toBe(PublishNamespace.id);
				const msg = await PublishNamespace.decode(request.reader, version);
				expect(msg.trackNamespace).toEqual(Path.from("room"));
				return request;
			};
			try {
				const first = await offered();
				await first.writer.u53(RequestOk.id);
				await new RequestOk().encode(first.writer, version);
				if (end === "reset") first.writer.reset(new StreamError(StreamCode.Cancel));
				else first.writer.close();
				await offered();
			} finally {
				connection.abort();
				origin.close();
			}
		});
	}
}
