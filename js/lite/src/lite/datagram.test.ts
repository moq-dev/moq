import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { Datagram, Datagrams, DatagramsOk, DatagramsUpdate } from "./datagram.ts";
import { Version } from "./version.ts";

async function buildBytes(write: (w: Writer) => Promise<void>): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream({
			write(chunk: Uint8Array) {
				// Copy: the Writer reuses an internal scratch buffer between varint writes,
				// so we have to snapshot each chunk before the next write overwrites it.
				chunks.push(new Uint8Array(chunk));
			},
		}),
	);
	await write(writer);
	writer.close();
	await writer.closed;
	const total = chunks.reduce((n, c) => n + c.byteLength, 0);
	const buf = new Uint8Array(total);
	let offset = 0;
	for (const c of chunks) {
		buf.set(c, offset);
		offset += c.byteLength;
	}
	return buf;
}

test("Datagram body round-trip", async () => {
	const original = new Datagram(7n, 42, new Uint8Array([1, 2, 3, 4, 5]));
	const bytes = await buildBytes((w) => original.encode(w, Version.DRAFT_05));
	const decoded = await Datagram.decode(new Reader(undefined, bytes), Version.DRAFT_05);
	expect(decoded.subscribe).toBe(original.subscribe);
	expect(decoded.sequence).toBe(original.sequence);
	expect(decoded.payload).toEqual(original.payload);
});

test("Datagram empty payload round-trip", async () => {
	const original = new Datagram(0n, 0, new Uint8Array());
	const bytes = await buildBytes((w) => original.encode(w, Version.DRAFT_05));
	const decoded = await Datagram.decode(new Reader(undefined, bytes), Version.DRAFT_05);
	expect(decoded.payload.byteLength).toBe(0);
});

test("Datagram rejects pre-Lite05 versions", async () => {
	const original = new Datagram(0n, 0, new Uint8Array([1]));
	await expect(buildBytes((w) => original.encode(w, Version.DRAFT_04))).rejects.toThrow(/datagrams not supported/);
});

test("Datagrams control message round-trip", async () => {
	const original = new Datagrams({
		id: 5n,
		broadcast: Path.from("alpha"),
		track: "video",
		maxLatency: 33,
	});
	const bytes = await buildBytes((w) => original.encode(w, Version.DRAFT_05));
	const decoded = await Datagrams.decode(new Reader(undefined, bytes), Version.DRAFT_05);
	expect(decoded.id).toBe(original.id);
	expect(decoded.track).toBe(original.track);
	expect(decoded.maxLatency).toBe(original.maxLatency);
});

test("DatagramsOk round-trip", async () => {
	const original = new DatagramsOk(33);
	const bytes = await buildBytes((w) => original.encode(w, Version.DRAFT_05));
	const decoded = await DatagramsOk.decode(new Reader(undefined, bytes), Version.DRAFT_05);
	expect(decoded.maxLatency).toBe(33);
});

test("DatagramsUpdate round-trip", async () => {
	const original = new DatagramsUpdate(0);
	const bytes = await buildBytes((w) => original.encode(w, Version.DRAFT_05));
	const decoded = await DatagramsUpdate.decode(new Reader(undefined, bytes), Version.DRAFT_05);
	expect(decoded.maxLatency).toBe(0);
});
