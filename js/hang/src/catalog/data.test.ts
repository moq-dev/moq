import { expect, test } from "bun:test";
import { modeSupported } from "./mode.ts";
import type { RelativeBroadcast } from "./path.ts";
import { RootSchema } from "./root.ts";

// The `json` and `binary` sections list application data tracks. Each entry says how to read the
// track, so a consumer needs the track name and nothing else about the application.

const catalog = {
	json: {
		tracks: {
			chat: { mode: "stream", schema: "https://example.com/chat.schema.json" },
			status: { broadcast: "./source", mode: "snapshot" },
		},
	},
	binary: {
		tracks: {
			thumbnail: { mode: "snapshot", mime: "image/jpeg" },
		},
	},
};

test("data tracks parse with their mode", () => {
	const parsed = RootSchema.parse(catalog);

	expect(parsed.json?.tracks.chat?.mode).toBe("stream");
	expect(parsed.json?.tracks.chat?.schema).toBe("https://example.com/chat.schema.json");

	expect(parsed.json?.tracks.status?.mode).toBe("snapshot");
	// Normalized like Rust path::Relative, the same as a media rendition's reference.
	expect(parsed.json?.tracks.status?.broadcast).toBe("source" as RelativeBroadcast);

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
	expect(() => RootSchema.parse({ json: { tracks: { chat: { schema: "chat" } } } })).toThrow();
});

test("an unrecognized mode survives verbatim", () => {
	// A relay reparses and republishes the catalog, so a track it cannot read must round-trip
	// rather than be corrupted. It just has to be skipped, not dropped.
	const parsed = RootSchema.parse({
		json: { tracks: { future: { mode: "windowed" } } },
	});

	const future = parsed.json?.tracks.future;
	expect(future?.mode).toBe("windowed");
	expect(modeSupported(future?.mode ?? "")).toBe(false);
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
