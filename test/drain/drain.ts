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
 * Exits 0 once the viewer has migrated, read groups on both relays with none missing in
 * between, and left A on its own; 1 otherwise.
 *
 * @module
 */
import * as net from "node:net";
import { parseArgs } from "node:util";
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
const HANDOVER = Moq.Time.Milli(2000);
// The viewer's latency budget, as a player sets one (the interop subscribers use the same).
// Following the route means resubscribing on B, and B has to subscribe upstream afresh once A
// drops its pull; the budget is what lets that resubscribe reach back to a group that was in
// flight across the swap instead of starting at the next one. With none, a group boundary
// landing inside the swap drops that group.
const MAX_DELAY = Moq.Time.Milli(1000);

const path = Moq.Path.from("drain");
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
const broadcast = published.createBroadcast(path);
const track = broadcast.createTrack(trackName, { timescale: Moq.Time.Timescale.MILLI });
broadcast.announce();
const publisher = new Moq.Connection({ url: new URL(`http://127.0.0.1:${bPort}/`), publish: published.consume() });

let lastPublished = -1;
const ticker = setInterval(() => {
	const group = track.appendGroup();
	group.writeString(String(group.sequence));
	group.close();
	lastPublished = group.sequence;
}, GROUP_INTERVAL_MS);

// ── viewer through the name ───────────────────────────────────────────────────
/** Which relay delivered each group: the generation of the broadcast it was read from. */
const seen = new Map<number, Set<number>>();
let generation = -1;
/** A group whose payload is not its sequence number: fails the run however the swap goes. */
let corrupt: string | undefined;

const watched = new Moq.Origin.Producer();
const viewer = new Moq.Connection({
	url: new URL(`http://127.0.0.1:${proxyPort}/`),
	consume: watched,
	goaway: { handover: HANDOVER },
});
const request = watched.request(path, { announced: true });

async function read(sub: Moq.Track.Subscriber, gen: number): Promise<void> {
	try {
		for (;;) {
			const group = await sub.recvGroup();
			if (!group) return;
			const text = await group.readString();
			if (text !== String(group.sequence)) {
				corrupt ??= `generation ${gen}: group ${group.sequence} carried ${text}`;
				return;
			}
			let gens = seen.get(group.sequence);
			if (!gens) {
				gens = new Set();
				seen.set(group.sequence, gens);
			}
			gens.add(gen);
		}
	} catch (err) {
		// The broadcast it was read from went away; its successor picks up from here.
		log(`generation ${gen} ended: ${err instanceof Error ? err.message : String(err)}`);
	}
}

// Follow whichever broadcast the path routes to, as a player does: subscribe to the new one and
// drop the old one each time the route changes.
// Subscribed before the first peek, so no route change can land between reading it and listening.
let current: { broadcast: Moq.Broadcast.Consumer; sub: Moq.Track.Subscriber } | undefined;
const follow = (active: Moq.Broadcast.Consumer | undefined) => {
	if (!active || active === current?.broadcast) return;
	generation++;
	log(`watching generation ${generation}`);
	const sub = active.track(trackName).subscribe({ maxDelay: MAX_DELAY });
	void read(sub, generation);
	current?.sub.close();
	current = { broadcast: active, sub };
};
const unfollow = request.active.subscribe(follow);
follow(request.active.peek());

const readOn = (gen: number) => [...seen.values()].filter((gens) => gens.has(gen)).length;
const newestOn = (gen: number) => Math.max(-1, ...[...seen].filter(([, gens]) => gens.has(gen)).map(([seq]) => seq));

let failure: Error | undefined;
try {
	await until(`${GROUPS_PER_RELAY} groups through relay A`, () => generation === 0 && readOn(0) >= GROUPS_PER_RELAY);
	if (a.dialed !== 1 || b.dialed !== 0) throw new Error(`expected one dial to A, saw A=${a.dialed} B=${b.dialed}`);

	log("withdrawing A from the name, then draining it");
	resolved = b;
	process.kill(aPid, "SIGTERM");

	// Counted past the last group A delivered: B's first answer can include recent groups the
	// viewer already had, which prove nothing about B being live.
	// Bounded well inside A's 20s drain window: a viewer that only reconnects once A cuts it off
	// at the deadline would otherwise pass, just late.
	await until(
		`${GROUPS_PER_RELAY} new groups through relay B`,
		() => generation >= 1 && newestOn(generation) >= newestOn(0) + GROUPS_PER_RELAY,
		HANDOVER * 3,
	);
	// The old session leaves at the handover cap, before A's own deadline would cut it.
	await until("the viewer to leave relay A", () => a.open === 0, HANDOVER * 3);

	if (generation !== 1) throw new Error(`expected exactly one migration, saw ${generation}`);
	if (a.dialed !== 1) throw new Error(`the viewer redialed the withdrawn relay (${a.dialed} dials)`);
	if (b.dialed < 1) throw new Error("the viewer never dialed relay B");

	const sequences = [...seen.keys()].sort((x, y) => x - y);
	const first = sequences[0] ?? 0;
	const last = sequences[sequences.length - 1] ?? 0;
	const missing: number[] = [];
	for (let seq = first; seq <= last; seq++) {
		if (!seen.has(seq)) missing.push(seq);
	}
	const both = sequences.filter((seq) => seen.get(seq)?.size === 2).length;
	log(`read groups ${first}..${last} (published up to ${lastPublished}); ${both} arrived from both relays`);
	if (missing.length > 0) throw new Error(`dropped groups across the migration: ${missing.join(", ")}`);
	if (corrupt) throw new Error(`corrupt group from ${corrupt}`);
	log("migrated without a dropped group");
} catch (err) {
	failure = err instanceof Error ? err : new Error(String(err));
} finally {
	unfollow();
	current?.sub.close();
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
