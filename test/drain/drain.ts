/**
 * Drives one relay drain end to end: a viewer watching a live track through relay A, behind a
 * stand-in for DNS, migrates to relay B when A drains, without missing a group.
 *
 * The "name" the viewer dials is a TCP proxy owned by this script. Pointing it at B before A
 * drains is the fleet's DNS withdrawal: nothing new resolves to A, while sessions already on A
 * stay put. A then gets SIGTERM, sends every session a GOAWAY, and the viewer redials the same
 * URL, which now lands on B. The publisher sits on B and A pulls it through the cluster, so both
 * relays carry the same track with the same group numbers.
 *
 *     bun drain.ts --a-port 4470 --b-port 4471 --proxy-port 4472 --a-pid 1234 --timeout 60
 *
 * The viewer subscribes once, at no latency budget, and reads the same subscription throughout:
 * the publisher mints an epoch, so B's route serves the same broadcast and the origin resumes the
 * track there from the first group the viewer lacks.
 *
 * Exits 0 once the viewer has migrated, read groups through both relays with none missing in
 * between, and left A on its own; 1 otherwise.
 *
 * @module
 */
import * as net from "node:net";
import { parseArgs } from "node:util";
import { Milli } from "@moq/clock";
import * as Moq from "@moq/net";

const { values } = parseArgs({
	options: {
		"a-port": { type: "string" },
		"b-port": { type: "string" },
		"proxy-port": { type: "string" },
		"a-pid": { type: "string" },
		timeout: { type: "string", default: "60" },
	},
});

const aPort = Number(values["a-port"]);
const bPort = Number(values["b-port"]);
const proxyPort = Number(values["proxy-port"]);
const aPid = Number(values["a-pid"]);
const timeoutMs = Number(values.timeout) * 1000;
if (![aPort, bPort, proxyPort, aPid, timeoutMs].every((n) => Number.isInteger(n) && n > 0)) {
	console.error("usage: drain.ts --a-port P --b-port P --proxy-port P --a-pid PID [--timeout S]");
	process.exit(2);
}

// Groups the viewer must read on each relay: enough to prove the track is live there,
// not merely that one group slipped through.
const GROUPS_PER_RELAY = 10;
// A new group every 100ms, one frame each carrying its own sequence number.
const GROUP_INTERVAL_MS = 100;
// How long the viewer's old session may keep serving after the GOAWAY. Well inside the relay's
// 20s drain window, so the relay's exit log can prove the viewer left on its own.
const HANDOVER = Milli(2000);

const path = Moq.Path.from("drain");
// Publisher epochs travel on the work-in-progress moq-lite-07 only, which nothing offers by default.
const versions = ["moq-lite-07-wip"];
const trackName = "seq";

function log(message: string) {
	console.error(`[drain] ${message}`);
}

// Resolve once `pred` holds, re-checking every tick; fails with `what` at the deadline.
async function until(what: string, pred: () => boolean, ms = timeoutMs): Promise<void> {
	const deadline = performance.now() + ms;
	while (!pred()) {
		if (performance.now() > deadline) throw new Error(`timed out waiting for ${what}`);
		await new Promise((resolve) => setTimeout(resolve, 20));
	}
}

// ── the name: a TCP proxy standing in for DNS ─────────────────────────────────
interface Backend {
	port: number;
	/** Connections ever sent here. */
	dialed: number;
	/** Connections still open. */
	open: number;
}
const a: Backend = { port: aPort, dialed: 0, open: 0 };
const b: Backend = { port: bPort, dialed: 0, open: 0 };
let resolved = a;

const proxy = net.createServer((client) => {
	const backend = resolved;
	backend.dialed++;
	backend.open++;
	const upstream = net.connect(backend.port, "127.0.0.1");
	client.pipe(upstream).pipe(client);
	let closed = false;
	const close = () => {
		if (closed) return;
		closed = true;
		backend.open--;
		client.destroy();
		upstream.destroy();
	};
	for (const socket of [client, upstream]) {
		socket.on("error", close);
		socket.on("close", close);
	}
});
await new Promise<void>((resolve, reject) => {
	proxy.once("error", reject);
	proxy.listen(proxyPort, "127.0.0.1", resolve);
});

// ── publisher on B ────────────────────────────────────────────────────────────
const published = new Moq.Origin.Producer();
const broadcastPublication = published.createBroadcast(path);
const broadcast = broadcastPublication.broadcast;
const track = broadcast.createTrack(trackName, { timescale: Moq.Time.Timescale.MILLI });
// The epoch is what says B's route serves the same broadcast as A's.
broadcastPublication.announce({ epoch: Moq.Epoch.mint() });
const publisher = new Moq.Connection({
	url: new URL(`http://127.0.0.1:${bPort}/`),
	publish: published.consume(),
	versions,
});

let lastPublished = -1;
const ticker = setInterval(() => {
	const group = track.appendGroup();
	group.writeFrame({
		payload: new TextEncoder().encode(String(group.sequence)),
		timestamp: Moq.Time.Timestamp.now(),
	});
	group.finish();
	lastPublished = group.sequence;
}, GROUP_INTERVAL_MS);

// ── viewer through the name ───────────────────────────────────────────────────
/** How many times each group was read: once, through whichever relay served it then. */
const seen = new Map<number, number>();
/** A group whose payload is not its sequence number: fails the run however the swap goes. */
let corrupt: string | undefined;
/** Why the subscription ended, if it did: it must outlive the migration. */
let ended: string | undefined;

const watched = new Moq.Origin.Producer();
const viewer = new Moq.Connection({
	url: new URL(`http://127.0.0.1:${proxyPort}/`),
	consume: watched,
	goaway: { handover: HANDOVER },
	versions,
});

// One request and one subscription, at the default zero budget, read for the whole run: the
// route moving from A to B re-sources the track underneath.
const request = watched.follow(path);
await until("the broadcast to resolve", () => request.active.peek() !== undefined);
const active = request.active.peek() as Moq.Broadcast.Consumer;
log("watching");
const sub = active.track(trackName).subscribe();
void (async () => {
	try {
		for (;;) {
			const group = await sub.recvGroup();
			if (!group) {
				ended = "the subscription finished";
				return;
			}
			const text = await group.readString();
			if (text !== String(group.sequence)) {
				corrupt ??= `group ${group.sequence} carried ${text}`;
				return;
			}
			seen.set(group.sequence, (seen.get(group.sequence) ?? 0) + 1);
		}
	} catch (err) {
		ended = err instanceof Error ? err.message : String(err);
	}
})();

const newest = () => Math.max(-1, ...seen.keys());

let failure: Error | undefined;
try {
	await until(`${GROUPS_PER_RELAY} groups through relay A`, () => seen.size >= GROUPS_PER_RELAY);
	if (a.dialed !== 1 || b.dialed !== 0) throw new Error(`expected one dial to A, saw A=${a.dialed} B=${b.dialed}`);

	log("withdrawing A from the name, then draining it");
	resolved = b;
	process.kill(aPid, "SIGTERM");

	// The old session leaves at the handover cap, before A's own deadline would cut it.
	await until("the viewer to leave relay A", () => a.open === 0, HANDOVER * 3);
	// Counted past the newest group when A went: only B can have delivered these. Bounded well
	// inside A's 20s drain window: a viewer that only reconnects once A cuts it off at the
	// deadline would otherwise pass, just late.
	const left = newest();
	await until(
		`${GROUPS_PER_RELAY} new groups through relay B`,
		() => newest() >= left + GROUPS_PER_RELAY,
		HANDOVER * 3,
	);

	if (a.dialed !== 1) throw new Error(`the viewer redialed the withdrawn relay (${a.dialed} dials)`);
	if (b.dialed < 1) throw new Error("the viewer never dialed relay B");
	if (request.active.peek() !== active) throw new Error("the request swapped its broadcast across the migration");
	if (ended) throw new Error(`the subscription ended across the migration: ${ended}`);

	const sequences = [...seen.keys()].sort((x, y) => x - y);
	const first = sequences[0] ?? 0;
	const last = sequences[sequences.length - 1] ?? 0;
	const missing: number[] = [];
	for (let seq = first; seq <= last; seq++) {
		if (!seen.has(seq)) missing.push(seq);
	}
	const repeated = sequences.filter((seq) => (seen.get(seq) ?? 0) > 1);
	log(`read groups ${first}..${last} (published up to ${lastPublished}); left A after ${left}`);
	if (missing.length > 0) throw new Error(`dropped groups across the migration: ${missing.join(", ")}`);
	if (repeated.length > 0) throw new Error(`groups handed out twice: ${repeated.join(", ")}`);
	if (corrupt) throw new Error(`corrupt group: ${corrupt}`);
	log("migrated without a dropped group");
} catch (err) {
	failure = err instanceof Error ? err : new Error(String(err));
} finally {
	sub.close();
	clearInterval(ticker);
	request.close();
	viewer.close();
	publisher.close();
	broadcast.close();
	proxy.close();
}

if (failure) {
	console.error(`error: ${failure.message}`);
	process.exit(1);
}
process.exit(0);
