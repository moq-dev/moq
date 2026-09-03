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

test("fields belonging to an unrecognized mode survive a round trip", () => {
	// Preserving the mode string alone is not enough: a future mode comes with fields describing it,
	// and a relay that reparsed and republished would otherwise strip them, leaving an entry nothing
	// can act on. Same guarantee the container schema gives an unknown container.
	const parsed = RootSchema.parse({
		json: { tracks: { future: { mode: "windowed", window: 10 } } },
		binary: { tracks: { future: { mode: "windowed", chunk: "4k" } } },
	});

	expect(parsed.json?.tracks.future).toMatchObject({ mode: "windowed", window: 10 });
	expect(parsed.binary?.tracks.future).toMatchObject({ mode: "windowed", chunk: "4k" });
});

test("a foreign data section does not fail the catalog", () => {
	// A section name is only reserved from the version that defines it, and `json` and `binary` are
	// generic enough that an application could already be using one. Dropping the section we can't
	// read keeps video and audio playable, instead of failing the whole catalog over that key.
	for (const section of ["json", "binary"] as const) {
		const parsed = RootSchema.parse({ video: { renditions: {} }, [section]: { messages: "chat" } });
		expect(parsed[section]).toBeUndefined();
		expect(parsed.video).toBeDefined();
	}
});

test("a foreign section with a non-object tracks value does not fail the catalog", () => {
	// The gate is the map's *shape*, not just its presence: an application section that happens to
	// carry its own `tracks` scalar is still someone else's key. Rust's `deserialize_section` gates
	// on `is_object`, so recognizing this as ours would make the two parsers disagree and take the
	// media sections down over a key we never owned.
	for (const section of ["json", "binary"] as const) {
		for (const tracks of [null, 3, "chat", [], true]) {
			const parsed = RootSchema.parse({ video: { renditions: {} }, [section]: { tracks } });
			expect(parsed[section]).toBeUndefined();
			expect(parsed.video).toBeDefined();
		}
	}
});
