import { expect, test } from "bun:test";
import { Reader, Writer } from "../stream.ts";
import * as Varint from "../varint.ts";
import { ProbeLevel, Role, Setup } from "./setup.ts";
import { Version } from "./version.ts";

// Parameter ids, duplicated from setup.ts so a typo there fails a test rather than
// silently agreeing with itself.
const PARAM_PROBE = 0x1n;
const PARAM_PATH = 0x2n;
const PARAM_ROLE = 0x3n;

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

// Hand-frame a SETUP carrying a single raw parameter, so we can feed the decoder values
// our own encoder would never produce (unknown levels, empty paths).
async function decodeParam(id: bigint, value: Uint8Array): Promise<Setup> {
	const body = await bytes(async (w) => {
		await w.u53(1); // parameter count
		await w.u62(id);
		await w.u53(value.byteLength);
		if (value.byteLength > 0) await w.write(value);
	});

	const framed = await bytes(async (w) => {
		await w.u53(body.byteLength); // Message size prefix
		await w.write(body);
	});

	return await Setup.decode(new Reader(undefined, framed), Version.DRAFT_05);
}

test("empty SETUP round-trips on draft-05", async () => {
	const got = await roundTrip(new Setup());
	expect(got.probe).toBe(ProbeLevel.None);
	expect(got.path).toBeUndefined();
	expect(got.role).toBe(Role.Both);
});

test("each probe level round-trips on draft-05", async () => {
	for (const probe of [ProbeLevel.None, ProbeLevel.Report, ProbeLevel.Increase]) {
		const got = await roundTrip(new Setup({ probe }));
		expect(got.probe).toBe(probe);
		expect(got.path).toBeUndefined();
	}
});

test("SETUP with path round-trips on draft-05", async () => {
	const got = await roundTrip(new Setup({ probe: ProbeLevel.Report, path: "/room/123" }));
	expect(got.probe).toBe(ProbeLevel.Report);
	expect(got.path).toBe("/room/123");
});

test("each role round-trips on draft-05", async () => {
	for (const role of [Role.Both, Role.Publisher, Role.Subscriber]) {
		const got = await roundTrip(new Setup({ role }));
		expect(got.role).toBe(role);
	}
});

test("SETUP carries role alongside probe and path", async () => {
	const got = await roundTrip(new Setup({ probe: ProbeLevel.Increase, path: "/room/123", role: Role.Publisher }));
	expect(got.probe).toBe(ProbeLevel.Increase);
	expect(got.path).toBe("/room/123");
	expect(got.role).toBe(Role.Publisher);
});

test("Both is the wire default, so it is omitted rather than encoded", async () => {
	// Both must be the absence of the parameter: an old server that never learned the
	// parameter has to decode a Both client back to Both.
	const body = await bytes((w) => new Setup({ role: Role.Both }).encode(w, Version.DRAFT_05));
	const empty = await bytes((w) => new Setup().encode(w, Version.DRAFT_05));
	expect(body).toEqual(empty);
});

test("unknown role falls back to Both", async () => {
	// Hand-frame a SETUP carrying an unrecognized role (99). The draft requires a receiver
	// that doesn't know the value to treat it as Both, so a newer client can't break us.
	const got = await decodeParam(PARAM_ROLE, Varint.encode(99));
	expect(got.role).toBe(Role.Both);
});

test("role 0 decodes as Both", async () => {
	const got = await decodeParam(PARAM_ROLE, Varint.encode(0));
	expect(got.role).toBe(Role.Both);
});

test("unknown probe level saturates to Increase", async () => {
	const got = await decodeParam(PARAM_PROBE, Varint.encode(99));
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
	await expect(decodeParam(PARAM_PATH, new Uint8Array())).rejects.toThrow();
});
