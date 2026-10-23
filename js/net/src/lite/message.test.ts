import { expect, test } from "bun:test";
import { Reader, Writer } from "../stream.ts";
import { AnnounceInit } from "./announce.ts";
import { Group } from "./group.ts";
import { Subscribe } from "./subscribe.ts";
import { Version } from "./version.ts";

async function prefix(size: number, version: Version): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
		version,
	);
	await writer.u53(size);
	writer.close();
	await writer.closed;
	return written[0] ?? new Uint8Array();
}

test("control messages past 65,535 bytes are refused at the length prefix", async () => {
	const version = Version.DRAFT_05;
	const wire = await prefix(65_536, version);
	await expect(Subscribe.decode(new Reader(undefined, wire, version), version)).rejects.toThrow("too large");
});

test("ANNOUNCE_INIT keeps room for a large initial set", async () => {
	const version = Version.DRAFT_02;
	const wire = await prefix(1024 * 1024, version);
	// Past the size check, so it fails waiting for a body that never arrives instead.
	await expect(AnnounceInit.decode(new Reader(undefined, wire, version), version)).rejects.not.toThrow("too large");
});

test("control message length 65,535 passes the prefix check", async () => {
	const version = Version.DRAFT_05;
	const wire = await prefix(65_535, version);
	await expect(Subscribe.decode(new Reader(undefined, wire, version), version)).rejects.not.toThrow("too large");
});

async function encodeGroup(group: Group, version: Version): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
		version,
	);
	await group.encode(writer, version);
	writer.close();
	await writer.closed;
	const wire = new Uint8Array(written.reduce((size, chunk) => size + chunk.byteLength, 0));
	let offset = 0;
	for (const chunk of written) {
		wire.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return wire;
}

// Lite-07 carries where a capped stream stops; lite-06 cannot, so it refuses one rather than
// let the FIN read as the group's end. An end at or below the start is malformed.
test("GROUP Frame End is lite-07 only", async () => {
	const capped = new Group({ subscribe: 1n, sequence: 7, frameStart: 4, frameEnd: 6 });
	const wire = await encodeGroup(capped, Version.DRAFT_07);
	const decoded = await Group.decode(new Reader(undefined, wire, Version.DRAFT_07), Version.DRAFT_07);
	expect([decoded.frameStart, decoded.frameEnd]).toEqual([4, 6]);

	await expect(encodeGroup(capped, Version.DRAFT_06)).rejects.toThrow("frame end");

	const inverted = await encodeGroup(new Group({ subscribe: 1n, sequence: 7, frameStart: 4 }), Version.DRAFT_07);
	inverted[inverted.length - 1] = 4;
	await expect(Group.decode(new Reader(undefined, inverted, Version.DRAFT_07), Version.DRAFT_07)).rejects.toThrow(
		"frame end",
	);
});
