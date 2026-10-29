import { expect, test } from "bun:test";
import { decode, encode } from "./catalog.ts";

function encodeJson(value: unknown): Uint8Array {
	return new TextEncoder().encode(JSON.stringify(value));
}

function decodeJson(raw: Uint8Array): Record<string, unknown> {
	return JSON.parse(new TextDecoder().decode(raw));
}

test("decodes a draft-00 catalog with a numeric version", () => {
	// Example 1 from draft-ietf-moq-msf-00, trimmed. Numeric version plus unmodeled
	// fields (targetLatency, generatedAt) which must be ignored.
	const catalog = decode(
		encodeJson({
			version: 1,
			generatedAt: 1746104606044,
			tracks: [
				{
					name: "1080p-video",
					namespace: "conference.example.com/conference123/alice",
					packaging: "loc",
					isLive: true,
					targetLatency: 2000,
					role: "video",
					codec: "av01.0.08M.10.0.110.09",
					width: 1920,
					height: 1080,
					framerate: 30,
					bitrate: 1500000,
				},
			],
		}),
	);

	expect(catalog.tracks).toHaveLength(1);
	expect(catalog.tracks[0].codec).toBe("av01.0.08M.10.0.110.09");
});

test("decodes a draft-00 catalog whose timeline tracks omit isLive", () => {
	// Example 8 from draft-ietf-moq-msf-00: mediatimeline tracks omit isLive/role/codec.
	const catalog = decode(
		encodeJson({
			version: 1,
			tracks: [
				{
					name: "history",
					packaging: "mediatimeline",
					mimetype: "application/json",
					depends: ["1080p-video"],
				},
				{
					name: "1080p-video",
					packaging: "loc",
					isLive: true,
					role: "video",
					codec: "av01.0.08M.10.0.110.09",
				},
			],
		}),
	);

	expect(catalog.tracks).toHaveLength(2);
	expect(catalog.tracks[0].isLive).toBe(false);
	expect(catalog.tracks[0].packaging).toBe("mediatimeline");
});

test("decodes a draft-01 catalog with a string version", () => {
	const catalog = decode(
		encodeJson({
			version: "draft-01",
			tracks: [{ name: "audio", packaging: "loc", isLive: true, role: "audio", codec: "opus" }],
		}),
	);

	expect(catalog.tracks[0].role).toBe("audio");
});

test("resolves draft-01 initRef into inline initData", () => {
	const catalog = decode(
		encodeJson({
			version: "draft-01",
			initDataList: [{ id: "v0", type: "inline", data: "AQID" }],
			tracks: [
				{ name: "video0", packaging: "cmaf", isLive: true, role: "video", codec: "avc1.640028", initRef: "v0" },
			],
		}),
	);

	expect(catalog.tracks[0].initData).toBe("AQID");
});

// Mirrors the Rust side: a malformed initRef must not decode as a track with no init data.
function initRefCatalog(entry: { type: string; data: string }, track: Record<string, unknown>): Uint8Array {
	return encodeJson({
		version: "draft-01",
		initDataList: [{ id: "v0", ...entry }],
		tracks: [{ name: "video0", packaging: "cmaf", isLive: true, role: "video", codec: "avc1.640028", ...track }],
	});
}

test("rejects a dangling initRef", () => {
	const raw = initRefCatalog({ type: "inline", data: "AQID" }, { initRef: "missing" });
	expect(() => decode(raw)).toThrow(/missing initData/);
});

test("rejects an initRef to a non-inline entry", () => {
	const raw = initRefCatalog({ type: "url", data: "https://example.com/init" }, { initRef: "v0" });
	expect(() => decode(raw)).toThrow(/unsupported type/);
});

test("rejects inline initData that disagrees with its initRef", () => {
	const raw = initRefCatalog({ type: "inline", data: "AQID" }, { initData: "BAUG", initRef: "v0" });
	expect(() => decode(raw)).toThrow(/conflicting/);
});

test("accepts inline initData that matches its initRef", () => {
	const catalog = decode(initRefCatalog({ type: "inline", data: "AQID" }, { initData: "AQID", initRef: "v0" }));
	expect(catalog.tracks[0].initData).toBe("AQID");
});

test("rejects a duplicate initDataList id", () => {
	// Mirrors the Rust side: keeping either entry would let two readers disagree on init bytes.
	const raw = encodeJson({
		version: "draft-01",
		initDataList: [
			{ id: "v", type: "inline", data: "AQID" },
			{ id: "v", type: "inline", data: "BAUG" },
		],
		tracks: [
			{ name: "video0", packaging: "cmaf", isLive: true, role: "video", codec: "avc1.640028", initRef: "v" },
		],
	});

	expect(() => decode(raw)).toThrow(/duplicate initData/);
});

test("rejects an unsupported numeric version", () => {
	// Mirrors the Rust side: any number other than 1 is rejected.
	expect(() => decode(encodeJson({ version: 2, tracks: [] }))).toThrow();
});

test("encode hoists and dedups init data, then round-trips", () => {
	const catalog = {
		tracks: [
			{ name: "a", packaging: "cmaf", isLive: true, role: "video", codec: "avc1.640028", initData: "AQID" },
			{ name: "b", packaging: "cmaf", isLive: true, role: "video", codec: "avc1.640028", initData: "AQID" },
		],
	};

	const wire = decodeJson(encode(catalog));
	const list = wire.initDataList as { id: string; type: string; data: string }[];
	expect(list).toHaveLength(1);
	expect(list[0].data).toBe("AQID");
	expect(wire.version).toBe("draft-01");

	const wireTracks = wire.tracks as { initRef?: string; initData?: string }[];
	for (const t of wireTracks) {
		expect(t.initRef).toBe(list[0].id);
		expect(t.initData).toBeUndefined();
	}

	const parsed = decode(encode(catalog));
	expect(parsed.tracks[0].initData).toBe("AQID");
	expect(parsed.tracks[1].initData).toBe("AQID");
});

test("encode emits isLive when omitted", () => {
	const wire = decodeJson(
		encode({
			tracks: [{ name: "history", packaging: "mediatimeline" }],
		}),
	);

	const wireTracks = wire.tracks as { isLive?: boolean }[];
	expect(wireTracks[0].isLive).toBe(false);

	const parsed = decode(encodeJson(wire));
	expect(parsed.tracks[0].isLive).toBe(false);
});

test("preserves SAP fields through decode and encode", () => {
	const catalog = decode(
		encodeJson({
			version: "draft-01",
			tracks: [
				{
					name: "video0",
					packaging: "cmaf",
					isLive: true,
					role: "video",
					codec: "avc1.640028",
					maxGrpSapStartingType: 1,
					maxObjSapStartingType: 2,
					jitter: 15.0,
				},
			],
		}),
	);

	expect(catalog.tracks[0].maxGrpSapStartingType).toBe(1);
	expect(catalog.tracks[0].maxObjSapStartingType).toBe(2);
	expect(catalog.tracks[0].jitter).toBe(15);

	const wire = decodeJson(encode(catalog));
	const wireTracks = wire.tracks as {
		maxGrpSapStartingType?: number;
		maxObjSapStartingType?: number;
		jitter?: number;
	}[];
	expect(wireTracks[0].maxGrpSapStartingType).toBe(1);
	expect(wireTracks[0].maxObjSapStartingType).toBe(2);
	expect(wireTracks[0].jitter).toBe(15);
});

test("preserves locmafVersion through decode and encode", () => {
	const catalog = decode(
		encodeJson({
			version: "draft-01",
			initDataList: [{ id: "a0", type: "inline", data: "AQID" }],
			tracks: [
				{
					name: "audio0",
					packaging: "locmaf",
					locmafVersion: "0.3",
					isLive: true,
					role: "audio",
					codec: "mp4a.40.2",
					initRef: "a0",
				},
			],
		}),
	);

	expect(catalog.tracks[0].locmafVersion).toBe("0.3");
	expect(catalog.tracks[0].initData).toBe("AQID");

	const wire = decodeJson(encode(catalog));
	const wireTracks = wire.tracks as { locmafVersion?: string }[];
	expect(wireTracks[0].locmafVersion).toBe("0.3");
});

test("preserves delay through decode and encode", () => {
	const catalog = decode(
		encodeJson({
			version: "draft-01",
			tracks: [
				{
					name: "video0",
					packaging: "loc",
					isLive: true,
					role: "video",
					codec: "avc1.640028",
					delay: 200,
				},
			],
		}),
	);

	expect(catalog.tracks[0].delay).toBe(200);

	const wire = decodeJson(encode(catalog));
	const wireTracks = wire.tracks as { delay?: number }[];
	expect(wireTracks[0].delay).toBe(200);
});

test("carries extension root members through decode and encode", () => {
	// An extension section (here `mpegts`) rides the catalog root untouched, and a
	// section this build has never heard of survives the same way.
	const catalog = decode(
		encodeJson({
			version: "draft-01",
			generatedAt: 1746104606044,
			tracks: [],
			mpegts: { program: { transportStreamId: 4660, programNumber: 1, pmtPid: 100 } },
			somethingElse: [1, 2, 3],
		}),
	);

	expect(Object.keys(catalog.ext ?? {})).toEqual(["mpegts", "somethingElse"]);

	const wire = decodeJson(encode(catalog));
	expect(wire.mpegts).toEqual({ program: { transportStreamId: 4660, programNumber: 1, pmtPid: 100 } });
	expect(wire.somethingElse).toEqual([1, 2, 3]);
	// `generatedAt` is MSF's own field, not an extension section, so it is not smuggled back.
	expect(wire.generatedAt).toBeUndefined();
});

test("refuses an extension section named after a reserved root field", () => {
	expect(() => encode({ tracks: [], ext: { tracks: "nope" } })).toThrow(/reserved root field/);
});

test("round-trips a __proto__ extension member as data", () => {
	// Assigning `__proto__` to an ordinary object invokes the prototype setter: the member
	// vanishes from the catalog and its value becomes the map's prototype. Built as raw JSON,
	// since `__proto__:` in an object literal is that same setter syntax and never lands as a key.
	const raw = '{"version":"draft-01","tracks":[],"__proto__":{"polluted":1},"ok":2}';
	const catalog = decode(new TextEncoder().encode(raw));

	const ext = catalog.ext ?? {};
	expect(Object.keys(ext).sort()).toEqual(["__proto__", "ok"]);
	expect(Object.getPrototypeOf(ext)).toBeNull();

	const wire = decodeJson(encode(catalog));
	expect(Object.keys(wire)).toContain("__proto__");
	expect(wire.ok).toBe(2);
});

test("caption role, label, language and codec survive the wire", () => {
	const track = {
		name: "captions.en",
		packaging: "loc",
		isLive: true,
		role: "caption",
		codec: "ttml",
		lang: "en",
		label: "English SDH",
	};
	expect(decode(encode({ tracks: [track] })).tracks).toEqual([track]);
});

test("preserves a track namespace through MSF decoding and encoding", () => {
	for (const version of [1, "draft-01"]) {
		const wire = encodeJson({
			version,
			tracks: [{ name: "video", namespace: "room/left", packaging: "loc", role: "video", codec: "vp8" }],
		});
		expect(decodeJson(encode(decode(wire))).tracks).toEqual([
			{ name: "video", namespace: "room/left", packaging: "loc", role: "video", codec: "vp8", isLive: false },
		]);
	}
});

test("refuses duplicate JSON members before schema validation", () => {
	for (const text of [
		'{"version":1,"version":"draft-01","tracks":[]}',
		'{"version":1,"tracks":[{"name":"a","name":"b","packaging":"loc"}]}',
		'{"version":1,"tracks":[],"extension":{"x":0,"x":1}}',
	]) {
		expect(() => decode(new TextEncoder().encode(text))).toThrow(/duplicate JSON object key/);
	}
});

test("refuses malformed Unicode in catalogs", () => {
	for (const text of ['\ufeff{"version":1}', '{"version":1,"extension":"\\ud800"}', '{"version":1,"\\udfff":0}']) {
		expect(() => decode(new TextEncoder().encode(text))).toThrow();
	}
	const prefix = new TextEncoder().encode('{"version":1,"extension":"');
	expect(() => decode(new Uint8Array([...prefix, 0xff, 0x22, 0x7d]))).toThrow();
});
