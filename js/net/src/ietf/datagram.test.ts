import { expect, test } from "bun:test";
import { accept, connect } from "../connection/index.ts";
import { ProtocolViolation, SessionCode } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as Origin } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream, encodeVarint } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { wireOf } from "../wire.ts";
import { Connection } from "./connection.ts";
import { ObjectDatagram } from "./datagram.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const ALL: IetfVersion[] = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22];

function single(publisherPriority?: number): ObjectDatagram {
	return new ObjectDatagram({
		trackAlias: 3n,
		groupId: 42n,
		publisherPriority,
		endOfGroup: true,
		properties: Uint8Array.of(0x10, 0x05),
		body: { payload: new TextEncoder().encode("hello") },
	});
}

async function rejects(bytes: Uint8Array, version: IetfVersion) {
	expect(await ObjectDatagram.decode(bytes, version).catch((err: unknown) => err)).toBeInstanceOf(ProtocolViolation);
}

test("roundtrips on every draft", async () => {
	for (const version of ALL) {
		const datagram = single(7);
		const bytes = datagram.encode(version);
		expect(bytes[0]).toBe(0x07); // properties, end of group, object 0
		expect(await ObjectDatagram.decode(bytes, version)).toEqual(datagram);
	}
});

test("an explicit object id and a status", async () => {
	const datagram = new ObjectDatagram({
		trackAlias: 1n,
		groupId: 2n,
		objectId: 0n,
		publisherPriority: 128,
		endOfGroup: false,
		body: { status: 0 },
	});
	for (const version of ALL) {
		const bytes = datagram.encode(version);
		expect(bytes[0]).toBe(0x20);
		expect(await ObjectDatagram.decode(bytes, version)).toEqual(datagram);
	}
});

test("rejects invalid types", async () => {
	for (const version of ALL) {
		// A status that ends the group, the reserved bit, a bit past the defined ones, and a
		// Type too wide for any of them.
		for (const kind of [0x22, 0x10, 0x40, 2 ** 40]) {
			const type = encodeVarint(kind, version);
			const bytes = new Uint8Array(type.byteLength + 4);
			bytes.set(type);
			bytes.set([0x01, 0x02, 0x03, 0x04], type.byteLength);
			await rejects(bytes, version);
		}
	}
});

/** A server session, so a datagram written by the client is received. */
async function session() {
	const version = Version.DRAFT_22;
	const pair = createMockTransportPair(ALPN.DRAFT_22);
	const control = await Stream.open(pair.server, { version });
	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		version,
		client: false,
	});
	const writable = pair.client.datagrams.writable;
	if (!writable) throw new Error("mock datagrams have no writable");
	return { pair, connection, version, writable };
}

test("an undefined object status closes the session", async () => {
	const { pair, connection, version, writable } = await session();
	try {
		const bytes = new ObjectDatagram({
			trackAlias: 1n,
			groupId: 4n,
			endOfGroup: false,
			body: { status: 1 },
		}).encode(version);
		const writer = writable.getWriter();
		await writer.write(bytes);
		const closed = await pair.server.closed;
		expect(closed.closeCode).toBe(SessionCode.ProtocolViolation);
	} finally {
		connection.abort();
	}
});

test("a defined object status does not close the session", async () => {
	const { pair, connection, version, writable } = await session();
	try {
		const bytes = new ObjectDatagram({
			trackAlias: 1n,
			groupId: 4n,
			endOfGroup: false,
			body: { status: 0 },
		}).encode(version);
		const writer = writable.getWriter();
		await writer.write(bytes);
		const open = Symbol("open");
		const result = await Promise.race([pair.server.closed, Bun.sleep(50).then(() => open)]);
		expect(result).toBe(open);
	} finally {
		connection.abort();
	}
});

test("a datagram track round-trips over moq-transport", async () => {
	const source = new Origin();
	const path = Path.from("live");
	const publication = source.createBroadcast(path);
	const datagrams = publication.broadcast.createDatagrams("audio", {
		timescale: Timescale.MILLI,
		priority: 3,
	});
	publication.announce();
	const pair = createMockTransportPair(ALPN.DRAFT_22);
	const url = new URL("https://localhost/datagrams");
	const peers = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server, publish: source.consume() }),
	]);
	const remote = wireOf(peers[0]).consume(path);
	const reader = remote.datagrams("audio");
	reader.setPriority(1);
	try {
		const info = await reader.info();
		while (datagrams.priority.peek() !== 1) await datagrams.priority.changed();
		expect(info.priority).toBe(3);
		expect(info.timescale).toBe(Timescale.MILLI);
		datagrams.insert({ sequence: 4n, timestamp: Timestamp.fromMillis(9), payload: new Uint8Array([7]) });
		const received = await reader.recv();
		expect(received?.sequence).toBe(4n);
		expect(received?.timestamp?.asMillis()).toBe(9);
		expect(received?.payload).toEqual(new Uint8Array([7]));
		datagrams.finish();
		expect(await reader.recv()).toBeUndefined();
	} finally {
		reader.close();
		await peers[0].close();
		await peers[1].close();
		source.close();
	}
});
