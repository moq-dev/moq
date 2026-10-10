import { readFileSync, writeFileSync } from "node:fs";
import * as Auth from "@moq/auth";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";
import { connect } from "./transport";

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
		Container.Legacy.encodeFrame(new TextEncoder().encode("compat-frame"), Moq.Time.Micro(1000001)),
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
		timestamp: Moq.Time.Timestamp.fromMicros(Moq.Time.Micro(1)),
	});
	group.close();
	await connection.closed;
} else throw new Error(`unknown compatibility client command: ${command}`);
