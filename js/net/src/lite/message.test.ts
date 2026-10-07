import { expect, test } from "bun:test";
import { Reader, Writer } from "../stream.ts";
import { AnnounceInit } from "./announce.ts";
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
