/**
 * The lite-07 answer checked over random grant changes, claims, and edits, interleaved with its
 * stream's sends, mirroring the Rust `lite::publisher::routed::tests`.
 *
 * Two promises, each judged as the peer reads the stream:
 *
 * - When a prefix's SYNCED goes out, every claim the grant shows under it has had its Start. The
 *   grant and the claims are those the answer last read; a fresh answer under them tells what
 *   the grant shows.
 * - A claim that leaves while the origin still advertises it ends as lost, never as withdrawn.
 * - A claim is shown only where its scope covers every path beneath it, so a request there never
 *   fails Unroutable for want of a route. The table files each claim as the origin does.
 *
 * `MOQ_ANSWER_SEED` and `MOQ_ANSWER_CASES` pick the cases.
 */

import { afterAll, beforeAll, expect, spyOn, test } from "bun:test";
import { type Getter, Once, Signal } from "@moq/signals";
import fc from "fast-check";
import type { Grant } from "../auth.ts";
import { ProtocolViolation } from "../error.ts";
import { WireRoute } from "../hop.ts";
import * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import type { Advertised, Advertisements } from "../wire.ts";
import type { AnnounceBroadcast } from "./announce.ts";
import type { AnnounceStream } from "./announce_stream.ts";
import { Answer } from "./answer.ts";
import { Version } from "./version.ts";

/** Where claims are advertised: some above what grants and prefixes name, and one hidden. */
const PATHS = ["a", "a/x", "a/q/x", "a/.h", "b", "b/c"].map((path) => Path.from(path));
/** The scopes claims are advertised with, which bound what each may serve. */
const SCOPES: (string[] | undefined)[] = [undefined, ["a/q/x"], ["a/x", "b/c"]];
/** The grants: widen, narrow, move, and revoke between them. */
const GRANTS: (string[] | undefined)[] = [undefined, [""], ["a"], ["b"], ["a/x", "a/q"], ["a/x"], []];
/** The prefixes the peer asks for. */
const PREFIXES = ["", "a", "a/x", "a/q", "b"].map((path) => Path.from(path));
/** The handle's own scope. */
const HANDLES: (string[] | undefined)[] = [undefined, ["a", "b"], ["a/q", "b"]];

function patterns(members: string[]): Path.Prefixes {
	return Path.Prefixes.parse(members);
}

/**
 * Where the origin files a claim at `path` under `members`: there when they cover it, and
 * otherwise at each of them beneath it, with how many segments beneath it each lies.
 */
function filed(path: Path.Valid, members: string[] | undefined): [Path.Valid, bigint][] {
	if (!members || patterns(members).matches(path)) return [[path, 0n]];
	const depth = Path.parts(path).length;
	return members
		.map((member) => Path.from(member))
		.filter((at) => Path.hasPrefix(path, at))
		.map((at) => [at, BigInt(Path.parts(at).length - depth)]);
}

function grant(index: number): Grant | undefined {
	const members = GRANTS[index];
	return members && { publish: patterns(members), subscribe: patterns([]) };
}

type Op =
	| { kind: "grant"; index: number }
	| { kind: "advertise"; path: number; scope: number }
	| { kind: "withdraw"; path: number }
	| { kind: "reprice"; path: number; cost: number }
	| { kind: "edit"; remove: number; add: [number, boolean][] }
	/** The oldest send in flight completes. */
	| { kind: "send" }
	/** The answer runs as far as it can without a send completing. */
	| { kind: "run" };

interface Case {
	handle: number;
	initial: number;
	claims: [number, number][];
	ops: Op[];
}

const op: fc.Arbitrary<Op> = fc.oneof(
	{ weight: 4, arbitrary: fc.nat(GRANTS.length - 1).map((index) => ({ kind: "grant", index }) as const) },
	{
		weight: 2,
		arbitrary: fc
			.tuple(fc.nat(PATHS.length - 1), fc.nat(SCOPES.length - 1))
			.map(([path, scope]) => ({ kind: "advertise", path, scope }) as const),
	},
	{ weight: 1, arbitrary: fc.nat(PATHS.length - 1).map((path) => ({ kind: "withdraw", path }) as const) },
	{
		weight: 3,
		arbitrary: fc
			.tuple(fc.nat(PATHS.length - 1), fc.integer({ min: 1, max: 3 }))
			.map(([path, cost]) => ({ kind: "reprice", path, cost }) as const),
	},
	{
		weight: 3,
		arbitrary: fc
			.tuple(fc.nat(255), fc.array(fc.tuple(fc.nat(PREFIXES.length - 1), fc.boolean()), { maxLength: 2 }))
			.map(([remove, add]) => ({ kind: "edit", remove, add }) as const),
	},
	{ weight: 5, arbitrary: fc.constant({ kind: "send" } as const) },
	{ weight: 3, arbitrary: fc.constant({ kind: "run" } as const) },
);

const cases: fc.Arbitrary<Case> = fc.record({
	handle: fc.nat(HANDLES.length - 1),
	initial: fc.nat(GRANTS.length - 1),
	claims: fc.array(fc.tuple(fc.nat(PATHS.length - 1), fc.nat(SCOPES.length - 1)), { maxLength: 5 }),
	ops: fc.array(op, { minLength: 1, maxLength: 32 }),
});

/** Let the answer's loop run until it waits on something outside it. */
async function settle() {
	for (let i = 0; i < 64; i++) await Promise.resolve();
}

/** A getter that remembers the value its reader last read. */
class Watched<T> {
	signal: Signal<T>;
	read: T;
	getter: Getter<T>;

	constructor(value: T) {
		this.signal = new Signal(value);
		this.read = value;
		const signal = this.signal;
		this.getter = {
			peek: () => {
				this.read = signal.peek();
				return this.read;
			},
			changed: signal.changed.bind(signal),
			subscribe: signal.subscribe.bind(signal),
		};
	}
}

/** The claims the peer holds, by Announce ID: each its path and instance. */
class Peer {
	live = new Map<bigint, [Path.Valid, Instance.Valid]>();
	#next = 0n;

	start(path: Path.Valid, instance: Instance.Valid) {
		this.live.set(this.#next++, [path, instance]);
	}

	end(id: bigint): [Path.Valid, Instance.Valid] {
		const held = this.live.get(id);
		if (!held) throw new Error(`ends ${id}, which is not live`);
		this.live.delete(id);
		return held;
	}

	/** Both ends drop every claim within a prefix the edit removed. */
	dropWithin(removed: Path.Valid[]) {
		for (const [id, [path]] of [...this.live]) {
			if (removed.some((prefix) => Path.hasPrefix(prefix, path))) this.live.delete(id);
		}
	}

	holds(): Set<string> {
		return new Set([...this.live.values()].map(([path, instance]) => `${path} ${instance}`));
	}
}

/** A stream whose sends complete only when the test says, recording each as it is made. */
function stream(onSend: (msgs: AnnounceBroadcast[]) => void, gated: boolean) {
	const pending: (() => void)[] = [];
	const fake = {
		send: (...msgs: AnnounceBroadcast[]) => {
			onSend(msgs);
			if (!gated) return Promise.resolve();
			return new Promise<void>((resolve) => pending.push(resolve));
		},
	} as unknown as AnnounceStream;
	return { stream: fake, pending };
}

function answer(
	advertised: Getter<Advertisements | undefined>,
	grant: Getter<Grant | undefined>,
	allowed: Path.Prefixes | undefined,
	stream: AnnounceStream,
) {
	return new Answer({
		version: Version.DRAFT_07,
		advertised,
		origin: { covers: () => true, allowed },
		grant,
		ready: Promise.resolve(),
		closing: new Once<true>(),
		stream,
	});
}

/** What a fresh answer under `grant` tells for `prefix` from `table`, once it settles. */
async function shown(
	table: Advertisements,
	grant: Grant | undefined,
	allowed: Path.Prefixes | undefined,
	prefix: Path.Valid,
	hidden: boolean,
): Promise<Set<string>> {
	const peer = new Peer();
	let synced = false;
	const { stream: fake } = stream((msgs) => {
		for (const msg of msgs) {
			if (msg.status === "start") peer.start(msg.suffix, msg.instance);
			else if (msg.status === "endedId") peer.end(msg.id);
			else if (msg.status === "synced") synced = true;
		}
	}, false);
	const granted = new Signal(grant);
	const fresh = answer(new Signal<Advertisements | undefined>(table), granted, allowed, fake);
	void fresh.request(false, [], [{ prefix, hidden }]);
	await settle();
	expect(synced).toBe(true);
	// What it settles to, judged again from scratch, so its own SYNCED proves nothing.
	granted.set(grant, true);
	await settle();
	fresh.close();
	return peer.holds();
}

async function run(c: Case) {
	const members = HANDLES[c.handle];
	const allowed = members && patterns(members);
	const table = new Watched<Advertisements | undefined>(new Map());
	const granted = new Watched<Grant | undefined>(grant(c.initial));
	const claims = new Map<number, { scope: number; route: WireRoute }>();
	let nextInstance = 0;
	const peer = new Peer();
	const prefixes = new Map<bigint, { path: Path.Valid; hidden: boolean }>();
	let nextPrefix = 0n;
	let edit: { remove: bigint[]; add: [bigint, Path.Valid, boolean][] } | undefined;
	// Each send's SYNCED checks, run once the send is recorded, outside the answer's call.
	const checks: Promise<void>[] = [];

	// One identity per claim, wherever it is filed, as the origin keeps it.
	const identities = new Map<number, object>();
	const publish = () => {
		const out = new Map<Path.Valid, Advertised[]>();
		for (const [index, { scope, route }] of claims) {
			const identity = identities.get(index) ?? {};
			identities.set(index, identity);
			for (const [at, below] of filed(PATHS[index], SCOPES[scope])) {
				const snap = { identity, route: below === 0n ? route : { ...route, above: below } };
				out.set(at, [...(out.get(at) ?? []), snap]);
			}
		}
		table.signal.set(out);
	};
	const advertise = (path: number, scope: number) => {
		if (claims.has(path)) return;
		const instance = Instance.parse(`i${++nextInstance}`);
		identities.delete(path);
		claims.set(path, { scope, route: WireRoute.normalize({ instance, epoch: 0n }) });
		publish();
	};

	const { stream: fake, pending } = stream((msgs) => {
		const advertised = new Set([...claims.values()].map((snap) => snap.route.instance));
		for (const msg of msgs) {
			switch (msg.status) {
				case "start": {
					peer.start(msg.suffix, msg.instance);
					const claim = [...claims.values()].find(({ route }) => route.instance === msg.instance);
					const members = claim && SCOPES[claim.scope];
					if (members && !patterns(members).matches(msg.suffix))
						throw new Error(`${msg.suffix} (${msg.instance}) is shown beyond its scope ${members}`);
					break;
				}
				case "change":
					break;
				case "endedId": {
					const [path, instance] = peer.end(msg.id);
					if (!msg.lost && advertised.has(instance))
						throw new Error(`${path} (${instance}) ended as withdrawn while still advertised`);
					break;
				}
				case "ok": {
					if (!edit) throw new Error("an ANNOUNCE_OK answers no edit");
					const removed = edit.remove.flatMap((id) => {
						const asked = prefixes.get(id);
						prefixes.delete(id);
						return asked ? [asked.path] : [];
					});
					peer.dropWithin(removed);
					for (const [id, path, hidden] of edit.add) prefixes.set(id, { path, hidden });
					edit = undefined;
					break;
				}
				case "synced": {
					const asked = prefixes.get(msg.prefix);
					if (!asked) throw new Error(`SYNCED for ${msg.prefix}, which is not in effect`);
					const read = table.read ?? new Map();
					const grant = granted.read;
					const held = peer.holds();
					checks.push(
						shown(read, grant, allowed, asked.path, asked.hidden).then((owed) => {
							for (const claim of owed) {
								if (!held.has(claim))
									throw new Error(`SYNCED for "${asked.path}" went out before ${claim}`);
							}
						}),
					);
					break;
				}
				default:
					throw new Error(`a claims-only peer is sent ${msg.status}`);
			}
		}
	}, true);

	for (const [path, scope] of c.claims) advertise(path, scope);
	const subject = answer(table.getter, granted.getter, allowed, fake);
	let running: Promise<void> | undefined;
	try {
		for (const step of c.ops) {
			switch (step.kind) {
				case "grant":
					granted.signal.set(grant(step.index));
					break;
				case "advertise":
					advertise(step.path, step.scope);
					break;
				case "withdraw":
					if (claims.delete(step.path)) publish();
					break;
				case "reprice": {
					const claim = claims.get(step.path);
					if (!claim) break;
					claims.set(step.path, { ...claim, route: { ...claim.route, cost: BigInt(step.cost) } });
					publish();
					break;
				}
				case "edit": {
					if (edit) break;
					const remove = [...prefixes.keys()].filter((_, bit) => step.remove & (1 << bit));
					// Only what a peer may ask for: no prefix twice, or at or below another in the set.
					const set = [...prefixes].filter(([id]) => !remove.includes(id)).map(([, asked]) => asked.path);
					const add: { prefix: Path.Valid; hidden: boolean }[] = [];
					for (const [index, hidden] of step.add) {
						const prefix = PREFIXES[index];
						if (set.some((other) => Path.hasPrefix(other, prefix) || Path.hasPrefix(prefix, other)))
							continue;
						set.push(prefix);
						add.push({ prefix, hidden });
					}
					running = subject.request(false, remove, add);
					edit = { remove, add: add.map(({ prefix, hidden }) => [nextPrefix++, prefix, hidden]) };
					break;
				}
				case "send":
					pending.shift()?.();
					break;
				case "run":
					break;
			}
			await settle();
		}
		// Then everything queued goes out.
		while (pending.length > 0) {
			pending.shift()?.();
			await settle();
		}
		await Promise.all(checks);
	} finally {
		subject.close();
		for (const release of pending) release();
		await running;
	}
}

function setting(name: string, fallback: number, min: number): number {
	const raw = process.env[name];
	if (raw !== undefined && !/^\d+$/.test(raw)) throw new Error(`${name} must be an integer`);
	const value = Number(raw ?? fallback);
	if (!Number.isInteger(value) || value < min || value > 0x7fffffff) {
		throw new Error(`${name} must be an integer between ${min} and 2147483647`);
	}
	return value;
}

// The answer logs every claim it tells.
let quiet: ReturnType<typeof spyOn> | undefined;
beforeAll(() => {
	quiet = spyOn(console, "debug").mockImplementation(() => {});
});
afterAll(() => quiet?.mockRestore());

test(
	"synced follows what the grant shows",
	async () => {
		const seed = setting("MOQ_ANSWER_SEED", 0x616e73, 0);
		const numRuns = setting("MOQ_ANSWER_CASES", 1024, 1);
		await fc.assert(fc.asyncProperty(cases, run), { seed, numRuns });
	},
	{ timeout: 3_600_000 },
);

// A claim is shown only where it is filed and the handle's scope shares a path, granted or not:
// one at `a` serving `a/q/x` and `a/r/y` is filed at each, so through a handle admitting `a/q/y`
// and `a/r/y` it serves nothing under `a/q`.
test("a claim is shown only where its filing and the handle's scope share a path", async () => {
	const identity = {};
	const route = WireRoute.normalize({ instance: Instance.parse("i1"), epoch: 0n, above: 2n });
	const table: Advertisements = new Map([
		[Path.from("a/q/x"), [{ identity, route }]],
		[Path.from("a/r/y"), [{ identity, route }]],
	]);
	const allowed = patterns(["a/q/y", "a/r/y"]);
	for (const grant of [undefined, { publish: patterns([""]), subscribe: patterns([]) }]) {
		expect(await shown(table, grant, allowed, Path.from("a/q"), false)).toEqual(new Set());
		expect(await shown(table, grant, allowed, Path.from("a/r"), false)).toEqual(new Set(["a/r/y i1"]));
	}
});

// A peer must wait for ANNOUNCE_OK before its next edit. One that does not is refused even while
// the answer is still telling a claim change ahead of the OK, rather than checked against the
// prefixes before the pending edit: there `a/x` would pass, nested below the pending `a`.
test("an edit sent before the last was answered is refused", async () => {
	const identity = {};
	const route = (cost: bigint) => WireRoute.normalize({ instance: Instance.parse("i1"), epoch: 0n, cost });
	const table = new Signal<Advertisements | undefined>(new Map([[Path.from("b"), [{ identity, route: route(1n) }]]]));
	const { stream: fake, pending } = stream(() => {}, true);
	const subject = answer(table, new Signal<Grant | undefined>(undefined), undefined, fake);
	const running = subject.request(false, [], [{ prefix: Path.from("b"), hidden: false }]);
	try {
		for (let i = 0; i < 8; i++) {
			await settle();
			pending.shift()?.();
		}
		expect(pending.length).toBe(0);

		// A change the answer tells before the next edit's OK, held in flight.
		table.set(new Map([[Path.from("b"), [{ identity, route: route(2n) }]]]));
		void subject.request(false, [], [{ prefix: Path.from("a"), hidden: false }]);
		await settle();
		expect(pending.length).toBe(1);
		// Not returned: toThrow would wait on the loop's promise, which settles only on close.
		expect(() => {
			void subject.request(false, [], [{ prefix: Path.from("a/x"), hidden: false }]);
		}).toThrow(ProtocolViolation);
	} finally {
		subject.close();
		// The pending edit still writes its answer as each send completes.
		while (pending.length > 0) {
			pending.shift()?.();
			await settle();
		}
		await running;
	}
});
