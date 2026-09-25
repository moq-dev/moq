/**
 * Native-JS (non-browser) interop subscriber: run the workspace `@moq/net` +
 * `@moq/hang` under a runtime with no native WebTransport, via moq's own
 * `@moq/web-transport` polyfill (a prebuilt NAPI QUIC/HTTP3 addon, the one piece
 * that comes from npm rather than this checkout). Runs under both node and bun.
 * Connect, find the video track in the .hang catalog, read it through the same
 * container consumer the browser player uses, and exit 0 as soon as a non-empty
 * frame arrives (1 on timeout). Subscribe-only: publishing media needs a
 * WebCodecs encoder a native JS runtime lacks.
 *
 *     node --import tsx subscribe.ts subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
 *
 * @module
 */
import { parseArgs } from "node:util";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

// globalThis.WebTransport = the polyfill (no-op if a native one already exists).
// @moq/net's connect() reads globalThis.WebTransport at call time, so this just
// has to run before run() below.
install();

// How stale a group may get before it is skipped, matching the Go and Python
// subscribers. A relay drops a superseded group (RESET_STREAM Old) rather than
// finish sending it, e.g. a cached group a fresh one lands right behind, so a
// subscriber has to move on to the next group instead of failing.
const MAX_AGE = Moq.Time.Milli(1000);

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
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({ url: new URL(url as string), consume: origin });
	let requested: Moq.Origin.Requesting | undefined;
	try {
		const path = Moq.Path.from(broadcast as string);
		requested = origin.request(path, { announced: true });
		let bc = requested.active.peek();
		while (!bc) {
			await requested.active.changed();
			bc = requested.active.peek();
		}

		// The .hang catalog lives on the "catalog.json" track. It's a @moq/json
		// snapshot+delta value, reconstructed by Json.Snapshot.Consumer. A lazy publisher may
		// announce video in a later update, so keep reading until one has it.
		const track = bc.track("catalog.json").subscribe({ priority: Catalog.PRIORITY.catalog });
		const catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track, schema: Catalog.RootSchema });
		let video: [string, Catalog.VideoConfig] | undefined;
		while (!video) {
			const root = await catalog.next();
			if (!root) throw new Error("catalog ended without a video track");
			video = Object.entries(root.video?.renditions ?? {})[0];
		}

		const [name, config] = video;
		let format: Container.Format;
		if (config.container.kind === "legacy") {
			format = new Container.Legacy.Format(config);
		} else if (config.container.kind === "loc") {
			format = new Container.Loc.Format("video");
		} else {
			throw new Error(`unsupported video container: ${JSON.stringify(config.container)}`);
		}

		const sub = bc.track(name).subscribe({ priority: 0, maxAge: MAX_AGE });
		const consumer = new Container.Consumer(sub, { format, maxAge: MAX_AGE });
		try {
			for (;;) {
				const next = await consumer.next();
				if (!next) break;
				const bytes = next.frame?.payload.byteLength ?? 0;
				if (bytes > 0) {
					// The harness judges success by this marker, not the exit code: the
					// @moq/web-transport NAPI addon can segfault during the runtime's exit
					// teardown after a frame has arrived (an upstream bug, seen under bun),
					// which would turn a real success into a signal exit.
					console.error(`received ${bytes} bytes from ${broadcast}`);
					return;
				}
			}
		} finally {
			consumer.close();
		}
		throw new Error("no frame data received");
	} finally {
		requested?.close();
		connection.close(); // returns void, not a promise
		origin.close();
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
