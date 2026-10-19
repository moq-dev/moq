import { readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import * as Auth from "@moq/auth";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";
import { connect } from "./transport";

// This one client runs against both the checkout's and the released packages, so it sticks to
// API both have: the group and frame primitives and the snapshot decoder, not the consumers above
// them, whose shapes follow the checkout.

// The released packages brand units under `Moq.Time`, the checkout's under @moq/clock: both are
// plain numbers at runtime, so cast rather than call either brand.
type FrameMicros = Parameters<typeof Container.Legacy.encodeFrame>[1];
type TimestampMicros = Parameters<typeof Moq.Time.Timestamp.fromMicros>[0];

/** Find the catalog's first video rendition, reading updates until one has it. */
async function video(bc: Moq.Broadcast.Consumer): Promise<[string, Catalog.VideoConfig]> {
	const track = bc.track("catalog.json").subscribe({ priority: Catalog.PRIORITY.catalog });
	const decoder = new Json.Snapshot.Decoder<Catalog.Root>({ schema: Catalog.RootSchema });
	for (;;) {
		const group = await track.recvGroup();
		if (!group) throw new Error("catalog ended without a video track");
		// Each group opens with a snapshot; the frames after it are deltas.
		let first = true;
		try {
			for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) {
				if (first) decoder.snapshot(frame.payload);
				else decoder.delta(frame.payload);
				first = false;
				const rendition = Object.entries(decoder.decode()?.video?.renditions ?? {})[0];
				if (rendition) return rendition;
			}
		} catch (err) {
			// The relay resets a group a newer snapshot supersedes; resync from the next.
			if (!(err instanceof Moq.Error.Stream)) throw err;
		}
	}
}

/** Print `received N bytes` once a video frame decodes, or record the group live demand fills. */
async function subscribe(url: URL, broadcast: string, trackFile?: string, trackName?: string) {
	install();
	const origin = new Moq.Origin.Producer();
	const connection = await connect({ url, consume: origin, websocket: { enabled: false } });
	const requested = origin.request(Moq.Path.from(broadcast), { announced: true });
	try {
		let bc = requested.active.peek();
		while (!bc) {
			await requested.active.changed();
			bc = requested.active.peek();
		}

		if (trackFile) {
			// Name a group that live demand is filling, so a FETCH of it never races eviction.
			const name = trackName ?? (await video(bc))[0];
			const group = await bc.track(name).subscribe({ priority: 0 }).recvGroup();
			if (!group) throw new Error(`${name} ended before its first group`);
			writeFileSync(trackFile, `${name}\n${group.sequence}\n`);
			return;
		}

		const [name, config] = await video(bc);
		if (config.container.kind !== "legacy") {
			throw new Error(`unsupported video container: ${JSON.stringify(config.container)}`);
		}
		const format = new Container.Legacy.Format("video");
		const track = bc.track(name).subscribe({ priority: 0 });
		for (let group = await track.recvGroup(); group; group = await track.recvGroup()) {
			try {
				for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) {
					const bytes = format.decode(frame.payload).reduce((n, f) => n + f.payload.byteLength, 0);
					if (bytes > 0) {
						// The harness judges success by this marker: the @moq/web-transport addon
						// can crash during exit teardown after a frame has arrived.
						console.error(`received ${bytes} bytes from ${broadcast}`);
						return;
					}
				}
			} catch (err) {
				// The relay resets a group a newer one supersedes; move on to the next.
				if (!(err instanceof Moq.Error.Stream)) throw err;
			}
		}
		throw new Error("no frame data received");
	} finally {
		requested.close();
		// Wait for the close to reach the relay before exit, so it is not timed out.
		await connection.close();
		await connection.closed;
		origin.close();
	}
}

const [command, ...args] = process.argv.slice(2);
if (command === "sign") {
	const key = Auth.Key.parse(readFileSync(args[0], "utf8"));
	console.log(await Auth.Key.sign(key, { root: "compat", publish: ["video/**"], subscribe: ["**"] }));
} else if (command === "verify") {
	const key = Auth.Key.parse(readFileSync(args[0], "utf8"));
	const claims = await Auth.Key.verify(key, readFileSync(args[1], "utf8").trim());
	if (
		claims.root !== "compat" ||
		JSON.stringify(claims.publish) !== '["video/**"]' ||
		JSON.stringify(claims.subscribe) !== '["**"]'
	) {
		throw new Error(`token scope changed: ${JSON.stringify(claims)}`);
	}
} else if (command === "decode") {
	Catalog.RootSchema.parse(JSON.parse(readFileSync(args[0], "utf8")));
	const format = new Container.Legacy.Format("video");
	const decoded = format.decode(new Uint8Array(readFileSync(args[1])));
	if (
		decoded.length !== 1 ||
		decoded[0].timestamp !== 1000001 ||
		Buffer.from(decoded[0].payload).toString() !== "compat-frame"
	) {
		throw new Error("legacy container decoded the wrong timestamp or payload");
	}
} else if (command === "encode") {
	const catalog = Catalog.RootSchema.parse({
		video: { renditions: { video: { codec: "avc3.42001e", container: { kind: "legacy" } } } },
		audio: { renditions: {} },
	});
	writeFileSync(args[0], JSON.stringify(catalog));
	writeFileSync(
		args[1],
		Container.Legacy.encodeFrame(new TextEncoder().encode("compat-frame"), 1000001 as FrameMicros),
	);
} else if (command === "publish") {
	install();
	const origin = new Moq.Origin.Producer();
	const broadcast = origin.createBroadcast(Moq.Path.from(args[1]));
	const track = broadcast.createTrack("data", { timescale: Moq.Time.Timescale.MICRO });
	broadcast.announce();
	const connection = await connect({
		url: new URL(args[0]),
		publish: origin.consume(),
		websocket: { enabled: false },
	});
	// Stopped by the harness with SIGTERM: close, so the relay sees a close rather than timing it out.
	process.once("SIGTERM", () => void connection.close());
	console.log("published");
	// `demand()` replaced the producer's own `used` after @moq/net 0.4.2, and this one
	// client runs against both packages.
	type Used = { peek(): boolean; changed(): Promise<unknown> };
	const used: Used = "demand" in track ? track.demand().used : (track as unknown as { used: Used }).used;
	while (!used.peek()) await used.changed();
	const group = track.appendGroup();
	group.writeFrame({
		payload: new TextEncoder().encode("compat-fetch"),
		timestamp: Moq.Time.Timestamp.fromMicros(1 as TimestampMicros),
	});
	group.close();
	await connection.closed;
} else if (command === "subscribe") {
	// The flags of the interop subscriber this stands in for.
	const { values } = parseArgs({
		args,
		options: {
			url: { type: "string" },
			broadcast: { type: "string" },
			timeout: { type: "string", default: "20" },
			"track-file": { type: "string" },
			track: { type: "string" },
		},
	});
	const timeout = Number.parseFloat(values.timeout ?? "") * 1000;
	if (!values.url || !values.broadcast || !(timeout > 0)) {
		throw new Error("usage: subscribe --url U --broadcast B [--timeout S] [--track-file F [--track T]]");
	}
	const timer = new Promise<never>((_, reject) =>
		setTimeout(() => reject(new Error("timed out waiting for data")), timeout),
	);
	try {
		await Promise.race([
			subscribe(new URL(values.url), values.broadcast, values["track-file"], values.track),
			timer,
		]);
		process.exit(0);
	} catch (err) {
		console.error(`error: ${err instanceof Error ? err.message : String(err)}`);
		process.exit(1);
	}
} else throw new Error(`unknown compatibility client command: ${command}`);
