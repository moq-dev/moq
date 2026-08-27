import { expect, test } from "bun:test";
import { compressionSupported } from "./compression.ts";
import { modeSupported } from "./mode.ts";
import { RootSchema } from "./root.ts";

// The `json` and `binary` sections list application data tracks. Each entry says how to read the
// track, so a consumer needs the track name and nothing else about the application.

const catalog = {
	json: {
		tracks: {
			chat: { mode: "stream", compression: "deflate", schema: "https://example.com/chat.schema.json" },
			status: { broadcast: "./source", mode: "snapshot" },
		},
	},
	binary: {
		tracks: {
			thumbnail: { mode: "snapshot", mime: "image/jpeg" },
		},
	},
};

test("data tracks parse with their mode and compression", () => {
	const parsed = RootSchema.parse(catalog);

	expect(parsed.json?.tracks.chat?.mode).toBe("stream");
	expect(parsed.json?.tracks.chat?.compression).toBe("deflate");
	expect(parsed.json?.tracks.chat?.schema).toBe("https://example.com/chat.schema.json");

	expect(parsed.json?.tracks.status?.mode).toBe("snapshot");
	// Normalized like Rust PathRelative, the same as a media rendition's reference.
	expect(parsed.json?.tracks.status?.broadcast).toBe("source");
	expect(parsed.json?.tracks.status?.compression).toBeUndefined();

	expect(parsed.binary?.tracks.thumbnail?.mode).toBe("snapshot");
	expect(parsed.binary?.tracks.thumbnail?.mime).toBe("image/jpeg");
});

test("a media-only catalog leaves the data sections undefined", () => {
	const parsed = RootSchema.parse({ video: { renditions: {} }, audio: { renditions: {} } });
	expect(parsed.json).toBeUndefined();
	expect(parsed.binary).toBeUndefined();
});

test("a track without a mode is rejected", () => {
	// There is no safe default: reading a stream as a snapshot silently drops every record but the
	// last, so a mode-less entry is malformed rather than assumed.
	expect(() => RootSchema.parse({ json: { tracks: { chat: { compression: "deflate" } } } })).toThrow();
});

test("an unrecognized mode or compression survives verbatim", () => {
	// A relay reparses and republishes the catalog, so a track it cannot read must round-trip
	// rather than be corrupted. It just has to be skipped, not dropped.
	const parsed = RootSchema.parse({
		json: { tracks: { future: { mode: "windowed", compression: "zstd" } } },
	});

	const future = parsed.json?.tracks.future;
	expect(future?.mode).toBe("windowed");
	expect(future?.compression).toBe("zstd");
	expect(modeSupported(future?.mode ?? "")).toBe(false);
	expect(compressionSupported(future?.compression ?? "")).toBe(false);
});

test("the known modes and compressions are recognized", () => {
	expect(modeSupported("snapshot")).toBe(true);
	expect(modeSupported("stream")).toBe(true);
	expect(compressionSupported("deflate")).toBe(true);
});
