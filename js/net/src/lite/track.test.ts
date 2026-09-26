import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { Milli } from "../time.ts";
import { infoDefaults } from "../track.ts";
import { Track, TrackInfo } from "./track.ts";
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

test("TrackInfo round-trips on draft-05", async () => {
	const info = new TrackInfo({
		priority: 7,
		maxAge: 2000,
		timescale: 90000,
	});
	const reader = new Reader(undefined, await bytes((w) => info.encode(w, Version.DRAFT_05)));
	const got = await TrackInfo.decode(reader, Version.DRAFT_05);
	expect(got.priority).toBe(7);
	expect(got.maxAge).toBe(2000);
	expect(got.timescale).toBe(90000);
});

test("TrackInfo defaults match cross-language wire bytes", async () => {
	const info = new TrackInfo(infoDefaults());
	expect(await bytes((w) => info.encode(w, Version.DRAFT_05))).toEqual(
		new Uint8Array([0x0c, 0x00, 0x00, 0xc0, 0x1f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x43, 0xe8]),
	);
});

test("Track request round-trips on draft-05", async () => {
	const msg = new Track(Path.from("room"), "video");
	const reader = new Reader(undefined, await bytes((w) => msg.encode(w, Version.DRAFT_05)));
	const got = await Track.decode(reader, Version.DRAFT_05);
	expect(got.broadcast).toBe(Path.from("room"));
	expect(got.track).toBe("video");
});

test("TRACK_INFO is rejected before draft-05", async () => {
	const info = new TrackInfo({ timescale: 90000 });
	await expect(bytes((w) => info.encode(w, Version.DRAFT_04))).rejects.toThrow();
});

test("TrackInfo round-trips valid boundary metadata", async () => {
	const info = new TrackInfo({
		priority: 255,
		maxAge: Number.MAX_SAFE_INTEGER - 1,
		timescale: Number.MAX_SAFE_INTEGER,
	});
	const reader = new Reader(undefined, await bytes((w) => info.encode(w, Version.DRAFT_05)));
	const got = await TrackInfo.decode(reader, Version.DRAFT_05);
	expect(got.priority).toBe(255);
	expect(got.maxAge).toBe(Number.MAX_SAFE_INTEGER - 1);
	expect(got.timescale).toBe(Number.MAX_SAFE_INTEGER);
});

test("TrackInfo refuses invalid fields at construction", () => {
	for (const fields of [
		{ priority: 256, timescale: 1000 },
		{ priority: -1, timescale: 1000 },
		{ priority: 1.5, timescale: 1000 },
		{ timescale: 0 },
		{ timescale: -1 },
		{ timescale: 1.5 },
		{ timescale: Number.NaN },
		{ timescale: Number.POSITIVE_INFINITY },
		{ timescale: Number.MAX_SAFE_INTEGER + 1 },
		{ timescale: 2 ** 62 },
		{ maxAge: -1, timescale: 1000 },
		{ maxAge: 1.5, timescale: 1000 },
		{ maxAge: Number.MAX_SAFE_INTEGER + 1, timescale: 1000 },
		{ maxAge: Number.NaN, timescale: 1000 },
		{ maxAge: Number.POSITIVE_INFINITY, timescale: 1000 },
	]) {
		expect(() => new TrackInfo(fields)).toThrow(RangeError);
	}
});

test("mutated TrackInfo fields emit no bytes on encode", async () => {
	for (const mutate of [
		(info: TrackInfo) => {
			info.priority = 256;
		},
		(info: TrackInfo) => {
			info.timescale = 0;
		},
		(info: TrackInfo) => {
			info.timescale = Number.MAX_SAFE_INTEGER + 1;
		},
		(info: TrackInfo) => {
			info.maxAge = Number.MAX_SAFE_INTEGER + 1;
		},
	]) {
		const info = new TrackInfo({ priority: 1, maxAge: 0, timescale: 1000 });
		mutate(info);
		const written: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
		);
		await expect(info.encode(writer, Version.DRAFT_05)).rejects.toThrow(RangeError);
		writer.close();
		await writer.closed;
		expect(written).toEqual([]);
	}
});

for (const version of [Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07]) {
	test(`optional max age survives ${version}`, async () => {
		for (const maxAge of [undefined, 0, 30_000, Number.MAX_SAFE_INTEGER]) {
			const encoded = await bytes((w) => new TrackInfo({ maxAge }).encode(w, version));
			const got = await TrackInfo.decode(new Reader(undefined, encoded), version);
			expect(got.maxAge).toBe(
				version !== Version.DRAFT_07 && maxAge === Number.MAX_SAFE_INTEGER ? undefined : maxAge,
			);
		}
	});
}

for (const version of [Version.DRAFT_05, Version.DRAFT_06]) {
	test(`legacy unlimited value fits the old u53 reader on ${version}`, async () => {
		const encoded = await bytes((w) => new TrackInfo({}).encode(w, version));
		const old = new Reader(undefined, encoded);
		await old.u53(); // message length
		await old.u8(); // priority
		if (version === Version.DRAFT_05) await old.bool();
		const maxAge = Milli(await old.u53());
		expect(maxAge).toBe(Milli(Number.MAX_SAFE_INTEGER));
		expect(infoDefaults({ maxAge }).maxAge).toBe(maxAge);
		expect(await old.u53()).toBe(1000);
	});
	test(`legacy unlimited range decodes without unsafe numeric conversion on ${version}`, async () => {
		const boundary = BigInt(Number.MAX_SAFE_INTEGER);
		for (const age of [boundary - 1n, boundary, 1n << 53n, 1n << 60n, (1n << 62n) - 1n]) {
			const payload = await bytes(async (w) => {
				await w.u8(0);
				if (version === Version.DRAFT_05) await w.bool(false);
				await w.u62(age);
				await w.u53(1000);
			});
			const encoded = await bytes(async (w) => {
				await w.u53(payload.length);
				await w.write(payload);
			});
			const info = await TrackInfo.decode(new Reader(undefined, encoded), version);
			expect(info.maxAge).toBe(age < boundary ? Number(age) : undefined);
		}
	});
}
