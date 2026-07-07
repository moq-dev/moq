import { expect, test } from "bun:test";
import { Reader, Writer } from "../stream.ts";
import * as Varint from "../varint.ts";
import { ProbeLevel, Setup } from "./setup.ts";
import { Version } from "./version.ts";

function concat(chunks: Uint8Array[]): Uint8Array {
	const total = chunks.reduce((sum, c) => sum + c.byteLength, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const c of chunks) {
		out.set(c, offset);
		offset += c.byteLength;
	}
	return out;
}

async function bytes(f: (w: Writer) => Promise<void>): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
	);
	await f(writer);
	writer.close();
	await writer.closed;
	return concat(written);
}

async function roundTrip(msg: Setup): Promise<Setup> {
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, Version.DRAFT_05)));
	const got = await Setup.decode(reader, Version.DRAFT_05);
	expect(await reader.done()).toBe(true);
	return got;
}

test("empty SETUP round-trips on draft-05", async () => {
	const got = await roundTrip(new Setup());
	expect(got.probe).toBe(ProbeLevel.None);
	expect(got.path).toBeUndefined();
});

test("each probe level round-trips on draft-05", async () => {
	for (const probe of [ProbeLevel.None, ProbeLevel.Report, ProbeLevel.Increase]) {
		const got = await roundTrip(new Setup(probe));
		expect(got.probe).toBe(probe);
		expect(got.path).toBeUndefined();
	}
});

test("SETUP with path round-trips on draft-05", async () => {
	const got = await roundTrip(new Setup(ProbeLevel.Report, "/room/123"));
	expect(got.probe).toBe(ProbeLevel.Report);
	expect(got.path).toBe("/room/123");
});

test("unknown probe level saturates to Increase", async () => {
	// Hand-frame a SETUP body carrying an unknown probe level (99): a 1-parameter bag
	// (PROBE id 0x1) whose value is the varint 99, prefixed with the Message size.
	const value = Varint.encode(99);
	const body = await bytes(async (w) => {
		await w.u53(1); // parameter count
		await w.u62(0x1n); // PARAM_PROBE
		await w.u53(value.byteLength);
		await w.write(value);
	});

	const framed = await bytes(async (w) => {
		await w.u53(body.byteLength); // Message size prefix
		await w.write(body);
	});

	const got = await Setup.decode(new Reader(undefined, framed), Version.DRAFT_05);
	expect(got.probe).toBe(ProbeLevel.Increase);
});

test("SETUP is rejected before draft-05", async () => {
	await expect(bytes((w) => new Setup().encode(w, Version.DRAFT_04))).rejects.toThrow();
});

test("SETUP decode is rejected before draft-05", async () => {
	const framed = await bytes((w) => new Setup().encode(w, Version.DRAFT_05));
	await expect(Setup.decode(new Reader(undefined, framed), Version.DRAFT_04)).rejects.toThrow();
});

test("empty path is rejected on decode", async () => {
	// Hand-frame a SETUP with a zero-length PATH parameter.
	const body = await bytes(async (w) => {
		await w.u53(1); // parameter count
		await w.u62(0x2n); // PARAM_PATH
		await w.u53(0); // zero-length value
	});
	const framed = await bytes(async (w) => {
		await w.u53(body.byteLength);
		await w.write(body);
	});
	await expect(Setup.decode(new Reader(undefined, framed), Version.DRAFT_05)).rejects.toThrow();
});
