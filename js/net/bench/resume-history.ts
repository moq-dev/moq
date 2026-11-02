/**
 * Sweep fronts, first-seen track names, and route replacements for long-lived logical fronts,
 * then count what each retired route leaves reachable once its readers closed, its session ended,
 * and every cache aged out.
 *
 * Every route is a real lite session over the in-memory transport, so a retired route that is
 * still reachable keeps its session, subscriber, transport, and broadcast handles. `stable` reads
 * the same names on every route, so only their first route ever records them; `churn` reads names
 * first answered by each route. Each case reads its names one way (info lookup, group fetch, or
 * subscription) and closes every reader. A route is replaced make-before-break, as a session
 * draining after GOAWAY is: the fronts serve the new route, then the old session ends. Only the
 * serving route may stay reachable.
 *
 * Counts come from weak references after full collections, so they are exact only under a precise
 * collector: run it in Chromium with `bench/browser/micro.ts --bench resume-history`, which
 * exposes `gc` and fails once a retired route stays reachable. Bun scans its stack conservatively,
 * so a dead reference there can survive a collection.
 */
import { Manual, Milli, Nano } from "@moq/clock";
import type { Producer as BroadcastProducer } from "../src/broadcast.ts";
import { accept } from "../src/connection/accept.ts";
import { connect } from "../src/connection/connect.ts";
import * as Epoch from "../src/epoch.ts";
import { ALPN_06 } from "../src/lite/version.ts";
import { createMockTransportPair } from "../src/mock.ts";
import { Producer as OriginProducer } from "../src/origin.ts";
import * as Path from "../src/path.ts";
import { Front } from "../src/resume.ts";
import { wireOf } from "../src/wire.ts";

const PAYLOAD = 4096;
const MAX_DELAY = Milli(1000);
/** Past every cache's age-out, including the 30 s window a closed track's cache drains over. */
const EXPIRY = Nano.fromMilli(Milli(60_000));
const url = new URL("https://localhost/resume-history");
type History = "stable" | "churn";
type Access = "lookup" | "fetch" | "subscribe";

/** Full collections, each from a fresh task so no frame of the caller keeps a target. */
async function collect(): Promise<void> {
	const gc = (globalThis as { gc?: () => void }).gc;
	if (!gc) throw new Error("resume-history needs a precise collector: run it with bench/browser/micro.ts");
	for (let pass = 0; pass < 3; pass++) {
		await new Promise<void>((resolve) => setTimeout(resolve, 0));
		gc();
	}
}

/**
 * Age out every cache and collect until what is reachable stops changing: teardown runs on host
 * tasks and clock timers, and either may arm the other.
 */
async function drain(clock: Manual, probes: Probe[]): Promise<void> {
	let last = "";
	for (let round = 0; round < 8; round++) {
		await clock.advance(EXPIRY);
		await collect();
		const reachable = JSON.stringify(count(probes));
		if (reachable === last) return;
		last = reachable;
	}
	throw new Error(`reachable routes never settled: ${last}`);
}

/** The JS heap in use, exact under Chromium's `--enable-precise-memory-info`. */
function heap(): number {
	const memory = (performance as { memory?: { usedJSHeapSize: number } }).memory;
	if (!memory) throw new Error("resume-history needs performance.memory");
	return memory.usedJSHeapSize;
}

/** What one route leaves behind, observed only weakly. */
interface Probe {
	session: WeakRef<object>;
	transport: WeakRef<object>;
	broadcasts: WeakRef<object>[];
	payloads: WeakRef<ArrayBuffer>[];
}

interface Retained {
	sessions: number;
	transports: number;
	broadcasts: number;
	/** Delivered payload backing stores still reachable, each counted once. */
	bytes: number;
}

function count(probes: Probe[]): Retained {
	const buffers = new Set<ArrayBuffer>();
	let [sessions, transports, broadcasts] = [0, 0, 0];
	for (const probe of probes) {
		if (probe.session.deref()) sessions++;
		if (probe.transport.deref()) transports++;
		for (const broadcast of probe.broadcasts) if (broadcast.deref()) broadcasts++;
		for (const payload of probe.payloads) {
			const buffer = payload.deref();
			if (buffer) buffers.add(buffer);
		}
	}
	let bytes = 0;
	for (const buffer of buffers) bytes += buffer.byteLength;
	return { sessions, transports, broadcasts, bytes };
}

const names = (history: History, route: number, count: number) =>
	Array.from({ length: count }, (_, index) => (history === "stable" ? `t${index}` : `r${route}t${index}`));

/** Read every name through `front` the chosen way, closing each reader, and keep the payloads weakly. */
async function read(front: Front, tracks: string[], access: Access, payloads: WeakRef<ArrayBuffer>[]): Promise<void> {
	for (const name of tracks) {
		const track = front.consumer.track(name);
		if (access === "lookup") {
			await track.info();
			continue;
		}
		const subscriber = access === "subscribe" ? track.subscribe({ maxDelay: MAX_DELAY }) : undefined;
		const group = subscriber ? await subscriber.recvGroup() : await track.fetchGroup(0);
		const frame = await group?.readFrame();
		group?.close();
		subscriber?.close();
		if (frame?.payload.byteLength !== PAYLOAD || frame.payload.some((byte) => byte !== 7))
			throw new Error(`${name}: wrong payload`);
		payloads.push(new WeakRef(frame.payload.buffer as ArrayBuffer));
	}
}

interface Case {
	fronts: number;
	names: number;
	routes: number;
	history: History;
	access: Access;
}

interface Result {
	/** Microseconds in `Front.serve` per front per route. */
	serveUs: number;
	/** Microseconds per read, including the session round trips it waits on. */
	readUs: number;
	retired: Retained;
	serving: Retained;
	/** JS heap the fronts and their serving route kept, against the same heap once concluded. */
	heldBytes: number;
	concluded: Retained;
}

/**
 * The only strong hold on the serving route, so dropping it leaves that route to the fronts.
 * Nothing about a route is returned: a suspended caller's dead locals can still pin a value.
 */
interface Routes {
	/** Ends the serving route's session and drops the table's handles on it. */
	close?: () => void;
	serveMs: number;
	readMs: number;
	probes: Probe[];
}

/** Connect a route, serve every front from it, end the previous one, and read through the fronts. */
async function open(
	routes: Routes,
	publisher: OriginProducer,
	paths: Path.Valid[],
	fronts: Front[],
	read: (front: Front, payloads: WeakRef<ArrayBuffer>[]) => Promise<void>,
): Promise<void> {
	const pair = createMockTransportPair(ALPN_06);
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server, publish: publisher.consume() }),
	]);
	const handles = paths.map((path) => wireOf(client).consume(path));
	const probe: Probe = {
		session: new WeakRef(client),
		transport: new WeakRef(pair.client),
		broadcasts: handles.map((handle) => new WeakRef(handle)),
		payloads: [],
	};
	routes.probes.push(probe);
	let start = performance.now();
	for (let index = 0; index < fronts.length; index++) fronts[index].serve(handles[index]);
	routes.serveMs += performance.now() - start;
	routes.close?.();
	routes.close = () => {
		for (const handle of handles) handle.close();
		client.abort();
		server.abort();
	};
	start = performance.now();
	for (const front of fronts) await read(front, probe.payloads);
	routes.readMs += performance.now() - start;
}

/** Close every front and the serving route, in a frame of its own so no temporary outlives it. */
function conclude(routes: Routes, fronts: Front[]): void {
	for (const front of fronts.splice(0)) front.consumer.close();
	routes.close?.();
	routes.close = undefined;
}

async function measure(shape: Case): Promise<Result> {
	// Cache age-out runs on the installed clock, so moving it expires whatever only waited on it.
	const clock = new Manual();
	const installed = clock.install();
	try {
		return await measureOn(clock, shape);
	} finally {
		installed.close();
	}
}

async function measureOn(clock: Manual, { fronts, names: perRoute, routes, history, access }: Case): Promise<Result> {
	// One publisher instance per front: every route carries the same broadcasts and tracks.
	const publisher = new OriginProducer();
	const paths = Array.from({ length: fronts }, (_, index) => Path.from(`room/${index}`));
	const broadcasts: BroadcastProducer[] = paths.map((path) => {
		const publication = publisher.createBroadcast(path);
		publication.announce({ epoch: Epoch.mint() });
		return publication.broadcast;
	});
	const payload = new Uint8Array(PAYLOAD).fill(7);
	for (const broadcast of broadcasts) {
		const all = new Set(Array.from({ length: routes }, (_, route) => names(history, route, perRoute)).flat());
		for (const name of all) {
			const track = broadcast.createTrack(name);
			const group = track.appendGroup();
			group.writeFrame({ payload });
			group.finish();
		}
	}

	const logical = paths.map(() => new Front());
	const state: Routes = { serveMs: 0, readMs: 0, probes: [] };
	const probes = state.probes;
	for (let route = 0; route < routes; route++) {
		const tracks = names(history, route, perRoute);
		await open(state, publisher, paths, logical, (front, payloads) => read(front, tracks, access, payloads));
	}

	await drain(clock, probes);
	const retired = count(probes.slice(0, -1));
	const serving = count(probes.slice(-1));
	const held = heap();

	// Control: concluding the fronts and ending the last session releases everything, so a
	// harness that could not see a release fails here rather than report zero.
	conclude(state, logical);
	for (const broadcast of broadcasts) broadcast.close();
	publisher.close();
	await drain(clock, probes);
	const concluded = count(probes);
	return {
		serveUs: (state.serveMs * 1000) / (fronts * routes),
		readUs: (state.readMs * 1000) / (fronts * routes * perRoute),
		retired,
		serving,
		heldBytes: Math.max(0, held - heap()),
		concluded,
	};
}

/** Print every case; a reachable retired route shows in its row, for the harness to judge. */
export async function run(): Promise<void> {
	// Keep protocol diagnostics out of the measured CSV.
	console.debug = () => {};
	console.log(
		"fronts,names,routes,history,access,retired_sessions,retired_transports,retired_broadcasts,retired_payload_bytes,serving_sessions,serve_us,read_us,size_bytes",
	);
	for (const access of ["lookup", "fetch", "subscribe"] as const)
		for (const history of ["stable", "churn"] as const)
			for (const fronts of [1, 8])
				for (const perRoute of [1, 8])
					for (const routes of [2, 8, 32]) {
						const result = await measure({ fronts, names: perRoute, routes, history, access });
						const { retired, serving, concluded } = result;
						const label = `${fronts} fronts, ${perRoute} names, ${routes} routes, ${history}, ${access}`;
						if (concluded.sessions || concluded.transports || concluded.broadcasts || concluded.bytes)
							throw new Error(`${label}: the control kept ${JSON.stringify(concluded)} after concluding`);
						if (serving.sessions !== 1 || serving.broadcasts !== fronts)
							throw new Error(`${label}: the serving route was not observed`);
						console.log(
							[
								fronts,
								perRoute,
								routes,
								history,
								access,
								retired.sessions,
								retired.transports,
								retired.broadcasts,
								retired.bytes,
								serving.sessions,
								result.serveUs.toFixed(2),
								result.readUs.toFixed(2),
								result.heldBytes,
							].join(","),
						);
					}
}

if (import.meta.main) await run();
