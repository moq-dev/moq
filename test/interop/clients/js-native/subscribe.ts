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
import { writeFileSync } from "node:fs";
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
const MAX_DELAY = Moq.Time.Milli(1000);

const { positionals, values } = parseArgs({
	allowPositionals: true,
	options: {
		url: { type: "string" },
		broadcast: { type: "string" },
		timeout: { type: "string", default: "20" },
		"track-file": { type: "string" },
		track: { type: "string" },
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

// The .hang catalog lives on the "catalog.json" track. It's a @moq/json snapshot+delta
// value, reconstructed by Json.Snapshot.Consumer. A lazy publisher may announce video in a
// later update, so keep reading until one has it.
async function video(bc: Moq.Broadcast.Consumer): Promise<[string, Catalog.VideoConfig]> {
	const track = bc.track("catalog.json").subscribe({ priority: Catalog.PRIORITY.catalog });
	const catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track, schema: Catalog.RootSchema });
	for (;;) {
		const root = (await catalog.latest())?.value;
		if (!root) throw new Error("catalog ended without a video track");
		const first = Object.entries(root.video?.renditions ?? {})[0];
		if (first) return first;
	}
}

async function run(): Promise<void> {
	const origin = new Moq.Origin.Producer();
	// A released-compat run pins its version through its own transport and runs anonymous.
	// Otherwise the grant arrives over AUTH, which only the work-in-progress moq-lite-07
	// carries and no client offers by default. The WebSocket fallback cannot offer it, so it
	// stays off.
	const compat = process.env.INTEROP_COMPAT_TRANSPORT;
	const connection: Moq.Connection.Established = compat
		? await (await import(compat)).connect({ url: new URL(url as string), consume: origin })
		: await Moq.Connection.connect({
				url: new URL(url as string),
				consume: origin,
				webtransport: { protocols: ["moq-lite-07-wip"] },
				websocket: { enabled: false },
			});
	// The grant the relay sent, in the Rust client's `auth granted` shape, so the harness can
	// check it against the token this cell minted.
	const printGrant = (grant: Moq.Auth.Grant | undefined) => {
		if (!grant) return;
		const publish = JSON.stringify(grant.publish);
		const subscribe = JSON.stringify(grant.subscribe);
		console.error(`auth granted publish=${publish} subscribe=${subscribe}`);
	};
	let unwatch = () => {};
	if (!compat) {
		printGrant(connection.auth.grant.peek());
		unwatch = connection.auth.grant.subscribe(printGrant);
	}
	let requested: Moq.Origin.Requesting | undefined;
	try {
		const path = Moq.Path.from(broadcast as string);
		requested = origin.request(path, { announced: true });
		let bc = requested.active.peek();
		while (!bc) {
			await requested.active.changed();
			bc = requested.active.peek();
		}

		if (values["track-file"]) {
			// Name a group that live demand is filling, so a FETCH of it never races eviction.
			// The track is `--track`, or the catalog's first video rendition.
			const name = values.track ?? (await video(bc))[0];
			const group = await bc.track(name).subscribe({ priority: 0 }).recvGroup();
			if (!group) throw new Error(`${name} ended before its first group`);
			writeFileSync(values["track-file"], `${name}\n${group.sequence}\n`);
			return;
		}

		const [name, config] = await video(bc);
		let format: Container.Format;
		if (config.container.kind === "legacy") {
			format = new Container.Legacy.Format(config);
		} else if (config.container.kind === "loc") {
			format = new Container.Loc.Format("video");
		} else {
			throw new Error(`unsupported video container: ${JSON.stringify(config.container)}`);
		}

		const sub = bc.track(name).subscribe({ priority: 0, maxDelay: MAX_DELAY });
		const consumer = new Container.Consumer(sub, { format, maxDelay: MAX_DELAY });
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
		unwatch();
		requested?.close();
		// Wait for the transport to report the close too, so it reaches the relay before
		// process.exit; otherwise the relay times the connection out.
		await connection.close();
		await connection.closed;
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
