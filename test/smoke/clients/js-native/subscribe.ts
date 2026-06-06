// Native-JS (non-browser) subscriber: run the published @moq/net + @moq/hang
// packages under a JS runtime that has no native WebTransport, using moq's own
// @moq/web-transport polyfill (a prebuilt NAPI QUIC/HTTP3 addon). Runs under both
// node and bun -- see the smoke harness.
//
// Connect, read the .hang catalog to find the video track, subscribe it, and
// exit 0 as soon as any non-empty frame arrives (1 on timeout).
//
//   node --import tsx subscribe.ts subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
//
// Subscribe-only: publishing media needs a WebCodecs encoder, which a native JS
// runtime doesn't have. Reading raw container frames needs no codec.
import { parseArgs } from "node:util";
import * as Catalog from "@moq/hang/catalog";
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
if (role !== "subscribe" || !url || !broadcast) {
	console.error("usage: subscribe.ts subscribe --url U --broadcast B [--timeout S]");
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
		const announced = connection.announced(path);
		try {
			for (;;) {
				const entry = await announced.next();
				if (!entry) throw new Error("connection closed before broadcast was announced");
				if (entry.active && Moq.Path.hasPrefix(path, entry.path)) break;
			}
		} finally {
			announced.close();
		}

		const bc = connection.consume(path);

		// The .hang catalog lives on the "catalog.json" track. A lazy publisher may
		// announce video in a later update, so keep reading frames until one has it.
		const catalog = bc.subscribe("catalog.json", Catalog.PRIORITY.catalog);
		let videoTrack: string | undefined;
		while (!videoTrack) {
			const root = await Catalog.fetch(catalog);
			if (!root) throw new Error("catalog ended without a video track");
			const renditions = root.video?.renditions;
			if (renditions) videoTrack = Object.keys(renditions)[0];
		}

		const video = bc.subscribe(videoTrack, 0);
		let total = 0;
		for (;;) {
			const group = await video.recvGroup();
			if (!group) break;
			for (;;) {
				const frame = await group.readFrame();
				if (!frame) break;
				total += frame.byteLength;
				if (total > 0) {
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
