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
	// fields (namespace, targetLatency, generatedAt) which must be ignored.
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
	expect(catalog.tracks[0].isLive).toBeUndefined();
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

test("leaves initData undefined for a dangling or non-inline initRef", () => {
	const catalog = decode(
		encodeJson({
			version: "draft-01",
			initDataList: [{ id: "v0", type: "url", data: "https://example.com/init" }],
			tracks: [
				{ name: "a", packaging: "cmaf", isLive: true, role: "video", codec: "avc1.640028", initRef: "missing" },
				{ name: "b", packaging: "cmaf", isLive: true, role: "video", codec: "avc1.640028", initRef: "v0" },
			],
		}),
	);

	expect(catalog.tracks[0].initData).toBeUndefined();
	expect(catalog.tracks[1].initData).toBeUndefined();
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
