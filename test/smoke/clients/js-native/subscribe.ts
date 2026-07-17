/**
 * Native-JS (non-browser) smoke subscriber: run the workspace `@moq/net` +
 * `@moq/hang` under a runtime with no native WebTransport, via moq's own
 * `@moq/web-transport` polyfill (a prebuilt NAPI QUIC/HTTP3 addon, the one piece
 * that comes from npm rather than this checkout). Runs under both node and bun.
 * Connect, find the video track in the .hang catalog, subscribe, and exit 0 as
 * soon as a non-empty frame arrives (1 on timeout). Subscribe-only: publishing
 * media needs a WebCodecs encoder a native JS runtime lacks.
 *
 *     node --import tsx subscribe.ts subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
 *
 * @module
 */
import { parseArgs } from "node:util";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

// globalThis.WebTransport = the polyfill (no-op if a native one already exists).
// @moq/net's connect() reads globalThis.WebTransport at call time, so this just
// has to run before run() below.
install();

const { positionals, values } = parseArgs({
	allowPositionals: true,
	options: {
		url: { type: "string" },
		broadcast: { type: "string" },
		timeout: { type: "string", default: "20" },
	},
});

const role = positionals[0];
const url = values.url;
const broadcast = values.broadcast;
const timeoutMs = Number.parseFloat(values.timeout ?? "20") * 1000;
if (role !== "subscribe" || !url || !broadcast || !Number.isFinite(timeoutMs) || timeoutMs <= 0) {
	console.error("usage: subscribe.ts subscribe --url U --broadcast B [--timeout S>0]");
	process.exit(2);
}

async function run(): Promise<void> {
	const connection = await Moq.Connection.connect(new URL(url as string));
	try {
		const path = Moq.Path.from(broadcast as string);

		// Wait for the broadcast to be announced before subscribing. Subscribing to a
		// track on a broadcast the publisher hasn't announced yet races the relay,
		// which resets the catalog stream (RESET_STREAM). The Rust API folds this
		// wait into consume(); the JS API leaves it to the caller. The outer timeout
		// below bounds how long we wait.
		//
		// `announced(path)` is scoped to `path`, so each entry's `path` is relative to it
		// (empty for the broadcast at `path` itself). Any active entry means a matching
		// broadcast is up, so wait for one.
		const announced = connection.announced(path);
		try {
			for (;;) {
				const entry = await announced.next();
				if (!entry) throw new Error("connection closed before broadcast was announced");
				if (entry.active) break;
			}
		} finally {
			announced.close();
		}

		const bc = connection.consume(path);

		// The .hang catalog lives on the "catalog.json" track. It's a @moq/json
		// snapshot+delta value, reconstructed by Json.Snapshot.Consumer. A lazy publisher may
		// announce video in a later update, so keep reading until one has it.
		const track = bc.subscribe("catalog.json", { priority: Catalog.PRIORITY.catalog });
		const catalog = new Json.Snapshot.Consumer<Catalog.Root>(track, { schema: Catalog.RootSchema });
		let videoTrack: string | undefined;
		while (!videoTrack) {
			const root = await catalog.next();
			if (!root) throw new Error("catalog ended without a video track");
			const renditions = root.video?.renditions;
			if (renditions) videoTrack = Object.keys(renditions)[0];
		}

		const video = bc.subscribe(videoTrack, { priority: 0 });
		let total = 0;
		for (;;) {
			const group = await video.recvGroup();
			if (!group) break;
			for (;;) {
				const frame = await group.readFrame();
				if (!frame) break;
				total += frame.payload.byteLength;
				if (total > 0) {
					// The harness judges success by this marker, not the exit code: the
					// @moq/web-transport NAPI addon can segfault during the runtime's exit
					// teardown after a frame has arrived (an upstream bug, seen under bun),
					// which would turn a real success into a signal exit.
					console.error(`received ${total} bytes from ${broadcast}`);
					return;
				}
			}
		}
		throw new Error("no frame data received");
	} finally {
		connection.close(); // returns void, not a promise
	}
}

const timeout = new Promise<never>((_, reject) =>
	setTimeout(() => reject(new Error("timed out waiting for data")), timeoutMs),
);

try {
	await Promise.race([run(), timeout]);
	process.exit(0);
} catch (err) {
	console.error(`error: ${err instanceof Error ? err.message : String(err)}`);
	process.exit(1);
}
