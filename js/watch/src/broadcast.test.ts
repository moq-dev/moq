import { describe, expect, it } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Path } from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "./broadcast";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await flush();
}

type Announcement = { path: Moq.Path.Valid; active: boolean };

// A connection whose announcement stream the test drives by hand.
class FakeAnnounced {
	#pending: Announcement[] = [];
	#waiting: Array<(entry: Announcement | undefined) => void> = [];

	push(entry: Announcement): void {
		const waiting = this.#waiting.shift();
		if (waiting) waiting(entry);
		else this.#pending.push(entry);
	}

	next(): Promise<Announcement | undefined> {
		const pending = this.#pending.shift();
		if (pending) return Promise.resolve(pending);
		return new Promise((resolve) => this.#waiting.push(resolve));
	}

	close(): void {}
}

function mockConnection(announced: FakeAnnounced): Moq.Connection.Established {
	return {
		discovery: true,
		announced: () => announced,
		announcedBroadcast: () => ({ active: new Signal(undefined), close: () => {} }),
	} as unknown as Moq.Connection.Established;
}

function video(codec: string, fields: Record<string, unknown> = {}): Catalog.VideoConfig {
	return Catalog.VideoConfigSchema.parse({ codec, container: { kind: "legacy" }, ...fields });
}

function renditions(broadcast: Broadcast): string[] {
	return Object.keys(broadcast.out.catalog.peek()?.video?.renditions ?? {}).sort();
}

describe("Broadcast cross-broadcast renditions", () => {
	it("hides a rendition until its broadcast is announced", async () => {
		// A transcoder under `public/` referencing a source under `private/`: a viewer scoped to
		// `public/` is never told the source exists, and selecting it would render nothing.
		const announced = new FakeAnnounced();
		const broadcast = new Broadcast({
			enabled: true,
			connection: new Signal(mockConnection(announced)),
			name: Path.from("public/transcode.hang"),
			catalogFormat: "manual",
			catalog: {
				video: {
					renditions: {
						local: video("avc1.64001e", { bitrate: 1_000_000 }),
						remote: video("avc1.640028", { bitrate: 9_000_000, broadcast: "../private/source" }),
					},
				},
			},
		});

		await settle();
		expect(renditions(broadcast)).toEqual(["local"]);

		// The source is announced, so it becomes usable without a new catalog.
		announced.push({ path: Path.from("private/source"), active: true });
		await settle();
		expect(renditions(broadcast)).toEqual(["local", "remote"]);

		// ...and withdrawn again when it goes away.
		announced.push({ path: Path.from("private/source"), active: false });
		await settle();
		expect(renditions(broadcast)).toEqual(["local"]);

		broadcast.close();
	});
});
