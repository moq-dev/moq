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

const abort = new AbortController();
let activeConnection: Awaited<ReturnType<typeof Moq.Connection.connect>> | undefined;

function closeActiveConnection(): void {
	const connection = activeConnection;
	activeConnection = undefined;
	connection?.close(); // returns void, not a promise
}

async function run(): Promise<void> {
	// Since @moq/net 0.4.0 a session doesn't consume broadcasts itself: it feeds the
	// peer's announcements into an origin, and consumers request paths from that.
	const origin = new Moq.Origin.Producer();
	// smoke.sh sets this on external relays that are not the WebSocket column.
	const websocket = process.env.SMOKE_WEBSOCKET === "0" ? { enabled: false as const } : undefined;
	const connection = await Moq.Connection.connect({
		url: new URL(url as string),
		consume: origin,
		signal: abort.signal,
		websocket,
	});
	activeConnection = connection;
	// Same shape as the Rust clients' log line, so the harness records the version.
	console.error(`connected version=${connection.version}`);

	// Wait for the broadcast to be announced before subscribing. Subscribing to a
	// track on a broadcast the publisher hasn't announced yet races the relay,
	// which resets the catalog stream (RESET_STREAM). `announced: true` folds that
	// wait into the request; the outer timeout below bounds it.
	const request = origin.request(Moq.Path.from(broadcast as string), { announced: true });
	try {
		let bc = request.active.peek();
		while (!bc) bc = await request.active.changed();

		// The .hang catalog lives on the Catalog.TRACK ("catalog.json") track, one
		// JSON snapshot per frame validated against RootSchema. (@moq/hang 0.3.0
		// dropped the Catalog.Consumer helper, so read the frames directly.) A lazy
		// publisher may announce video in a later update, so keep pulling until one
		// carries a video track.
		const catalog = bc.track(Catalog.TRACK).subscribe({ priority: Catalog.PRIORITY.catalog });
		let videoTrack: string | undefined;
		while (!videoTrack) {
			const group = await catalog.recvGroup();
			if (!group) throw new Error("catalog ended without a video track");
			const frame = await group.readFrame();
			if (!frame) continue;
			const root = Catalog.RootSchema.parse(JSON.parse(new TextDecoder().decode(frame.payload)));
			const renditions = root.video?.renditions;
			if (renditions) videoTrack = Object.keys(renditions)[0];
		}

		const video = bc.track(videoTrack).subscribe({ priority: 0 });
		let total = 0;
		for (;;) {
			const group = await video.recvGroup();
			if (!group) break;
			try {
				for (;;) {
					const frame = await group.readFrame();
					if (!frame) break;
					total += frame.payload.byteLength;
					if (total > 0) {
						console.error(`received ${total} bytes from ${broadcast}`);
						return;
					}
				}
			} catch (err) {
				// A relay may reset a group stream it has given up on (e.g. a stale
				// group once a newer one starts). Players skip to the next group, so
				// do the same; the track itself failing still ends in recvGroup().
				console.error(`group reset, waiting for the next: ${err instanceof Error ? err.message : String(err)}`);
			}
		}
		throw new Error("no frame data received");
	} finally {
		request.close();
		if (activeConnection === connection) closeActiveConnection();
		origin.close();
	}
}

let timeoutId: ReturnType<typeof setTimeout> | undefined;
const timeout = new Promise<never>((_, reject) => {
	timeoutId = setTimeout(() => {
		const err = new Error("timed out waiting for data");
		abort.abort(err);
		closeActiveConnection();
		reject(err);
	}, timeoutMs);
});

// Let pending napi-rs cleanup finish instead of forcing environment teardown
// while native reads may still be releasing references.
try {
	await Promise.race([run(), timeout]);
} catch (err) {
	// AggregateError from Promise.any has an empty message; the cause is in .errors.
	const message =
		err instanceof AggregateError
			? err.errors.map((e) => (e instanceof Error ? e.message : String(e))).filter(Boolean).join("; ")
			: err instanceof Error
				? err.message
				: String(err);
	console.error(`error: ${message || String(err)}`);
	process.exitCode = 1;
} finally {
	if (timeoutId !== undefined) clearTimeout(timeoutId);
}
