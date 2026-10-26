// SPIKE (not for merge): what @moq/net does with sparse group IDs, the start PTS in the
// track's timescale (2 s GOPs at 1 kHz: 0, 2000, 4000, ...), against today's contiguous IDs.
// Run: bun test src/time-addressing-spike.test.ts
import { afterEach, mock, spyOn, test } from "bun:test";
import { Milli } from "@moq/clock";
import { connect as connectSession, accept as acceptSession } from "./connection/index.ts";
import { Producer as GroupProducer } from "./group.ts";
import * as Ietf from "./ietf/index.ts";
import { Publisher as IetfPublisher } from "./ietf/publisher.ts";
import * as Lite from "./lite/index.ts";
import { Publisher as LitePublisher } from "./lite/publisher.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Timescale, Timestamp } from "./time.ts";
import { wireOf } from "./wire.ts";

afterEach(() => mock.restore());

const url = new URL("https://localhost:4443/test");
const GOP = 2000;
const COUNT = 4;
const WAIT = 3000;
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

function publishGroups(track: ReturnType<ReturnType<OriginProducer["createBroadcast"]>["createTrack"]>, ids: number[], stride: number) {
	for (const id of ids) {
		const pts = (id / stride) * GOP;
		const group = new GroupProducer(id);
		track.writeGroup(group);
		group.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.fromMillis(pts) });
		group.writeFrame({ payload: new Uint8Array([2]), timestamp: Timestamp.fromMillis(pts + GOP / 2) });
		group.close();
	}
}

async function race<T>(p: Promise<T>): Promise<T | "stalled"> {
	return Promise.race([p, sleep(WAIT).then(() => "stalled" as const)]);
}

for (const protocol of [Lite.ALPN_06, Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_18]) {
	for (const stride of [1, GOP]) {
		for (const mode of ["history", "live"] as const) {
			for (const ordered of [false, true]) {
				test(`SPIKE ${protocol} stride=${stride} ${mode} ordered=${ordered}`, async () => {
					let fetches = 0;
					const liteFetch = LitePublisher.prototype.runFetch;
					spyOn(LitePublisher.prototype, "runFetch").mockImplementation(function (this: LitePublisher, ...args) {
						fetches++;
						return liteFetch.apply(this, args);
					});
					const ietfFetch = IetfPublisher.prototype.runFetch;
					spyOn(IetfPublisher.prototype, "runFetch").mockImplementation(function (this: IetfPublisher, ...args) {
						fetches++;
						return ietfFetch.apply(this, args);
					});

					const pair = createMockTransportPair(protocol);
					const origin = new OriginProducer();
					const [client, server] = await Promise.all([
						connectSession({ url, transport: pair.client }),
						acceptSession({ transport: pair.server, url, publish: origin.consume() }),
					]);
					const broadcast = origin.createBroadcast(Path.from("b"));
					broadcast.announce();
					const track = broadcast.createTrack("t", { timescale: Timescale.MILLI, maxAge: Milli(1 << 30) });
					const ids = Array.from({ length: COUNT }, (_, i) => i * stride);
					if (mode === "history") publishGroups(track, ids, stride);

					const remote = wireOf(client).consume(Path.from("b"));
					const sub = remote.track("t").subscribe({ maxDelay: Milli(1 << 30), groups: { start: { included: 0 } } });
					const start = performance.now();
					const got: number[] = [];
					let problem: string | undefined;
					const reader = ordered ? sub.ordered() : undefined;
					const writer = (async () => {
						if (mode !== "live") return;
						for (const id of ids) {
							await sleep(20);
							publishGroups(track, [id], stride);
						}
					})();
					while (got.length < COUNT) {
						const next = await race(reader ? reader.nextGroup() : sub.recvGroup());
						if (next === "stalled") {
							problem = "stalled";
							break;
						}
						if (!next) {
							problem = "ended";
							break;
						}
						got.push(next.sequence);
					}
					await writer;
					const ms = Math.round(performance.now() - start - (problem === "stalled" ? WAIT : 0));
					console.log(
						`SPIKE-JS ${protocol.padEnd(17)} stride=${String(stride).padEnd(4)} ${mode.padEnd(7)} ordered=${String(ordered).padEnd(5)} got=${JSON.stringify(got)} problem=${problem} ms=${ms} fetches=${fetches}`,
					);
					remote.close();
					broadcast.close();
					client.abort();
					server.abort();
				});
			}
		}
	}
}
