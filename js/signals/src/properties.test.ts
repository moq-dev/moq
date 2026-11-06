import { expect, test } from "bun:test";
import fc from "fast-check";
import { Effect, Signal } from "./index.ts";

// Class instances compare by identity, so two boxes holding the same value are never equal.
class Box {
	value = 0;
}
const boxes = [new Box(), new Box()];

// A small vocabulary, so independently generated values are often structurally equal
// but distinct references. Leaning on `undefined` makes a renamed key whose value reads the
// same either way (`{ a: undefined }` to `{ b: undefined }`) come up often.
const leaf = fc.oneof(
	{ arbitrary: fc.constant(undefined), weight: 3 },
	fc.constantFrom<unknown>(0, -0, Number.NaN, ...boxes),
);
const value: fc.Arbitrary<unknown> = fc.letrec<{ value: unknown }>((tie) => ({
	value: fc.oneof(
		{ maxDepth: 2, depthSize: "small" },
		leaf,
		fc.array(tie("value"), { maxLength: 2 }),
		fc.sparseArray(tie("value"), { maxLength: 3 }),
		fc.dictionary(fc.constantFrom("a", "b"), tie("value"), { maxKeys: 2 }),
	),
})).value;

// The reference equality: two values are equal when they print the same structure.
// Plain objects ignore key order; arrays keep their length and holes; anything else
// (class instances) is its identity. Numbers use SameValueZero, like a Set.
const ids = new Map<object, number>();
function shape(v: unknown): string {
	if (typeof v !== "object" || v === null) return Object.is(v, -0) ? "number:0" : `${typeof v}:${String(v)}`;
	const proto = Object.getPrototypeOf(v);
	if (proto === Array.prototype) {
		const items = Array.from({ length: (v as unknown[]).length }, (_, i) =>
			i in v ? shape((v as unknown[])[i]) : "hole",
		);
		return `[${items.join(",")}]`;
	}
	if (proto === Object.prototype) {
		const entries = Object.keys(v)
			.sort()
			.map((k) => `${JSON.stringify(k)}=${shape((v as Record<string, unknown>)[k])}`);
		return `{${entries.join(",")}}`;
	}
	if (!ids.has(v)) ids.set(v, ids.size);
	return `#${ids.get(v)}`;
}

type Kind = "subscribe" | "changed" | "effect";

// `clone` copies the value with fresh references, which must not notify. `rename` moves a
// plain object's first key to an unused one and `grow` appends a hole to an array. Both change
// the structure, so both must notify even when the moved value is `undefined` and holes read
// as `undefined`.
type Edit = "clone" | "rename" | "grow";

const isPlain = (v: unknown): v is Record<string, unknown> =>
	typeof v === "object" && v !== null && Object.getPrototypeOf(v) === Object.prototype;

function clone(v: unknown): unknown {
	if (Array.isArray(v)) {
		const copy: unknown[] = new Array(v.length);
		for (const i of Object.keys(v)) copy[Number(i)] = clone(v[Number(i)]);
		return copy;
	}
	if (isPlain(v)) return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, clone(x)]));
	return v;
}

function edit(v: unknown, kind: Edit): unknown {
	const copy = clone(v);
	if (kind === "grow" && Array.isArray(copy)) copy.length++;
	if (kind === "rename" && isPlain(copy)) {
		const [from] = Object.keys(copy);
		const to = ["a", "b", "c"].find((k) => !Object.hasOwn(copy, k));
		if (from !== undefined && to !== undefined) {
			copy[to] = copy[from];
			delete copy[from];
		}
	}
	return copy;
}

type Op =
	| { op: "set"; value: unknown; notify: boolean | undefined }
	// Sets a near-copy of the current value, where equality bugs hide.
	| { op: "edit"; edit: Edit }
	| { op: "listen"; kind: Kind }
	// Picks any listener, including a gone one, since disposing twice must be harmless.
	| { op: "unlisten"; index: number };

const op: fc.Arbitrary<Op> = fc.oneof(
	fc.record({ op: fc.constant("set" as const), value, notify: fc.constantFrom(undefined, true, false) }),
	fc.record({ op: fc.constant("edit" as const), edit: fc.constantFrom<Edit>("clone", "rename", "grow") }),
	fc.record({ op: fc.constant("listen" as const), kind: fc.constantFrom<Kind>("subscribe", "changed", "effect") }),
	fc.record({ op: fc.constant("unlisten" as const), index: fc.nat() }),
);

// A listener's expected deliveries. An effect is pending until its run subscribes it, and a
// listener is gone once disposed or, for `changed`, once it fired; gone ones must stay quiet.
type Listener = { kind: Kind; seen: unknown[]; pending: boolean; gone: boolean };

// The reference model of the notification contract. A write captures the value it replaces,
// and the flush at the end of the microtask delivers the final value to every listener when
// it differs from that capture or the write was forced. A write while nobody listens drops
// the capture, so nothing is delivered for it.
type Model = {
	value: unknown;
	base?: { value: unknown };
	force: boolean;
	listeners: Map<number, Listener>;
	// Microtasks in queue order: the signal's flush, or an effect's run.
	queue: ("flush" | number)[];
};

type Real = {
	signal: Signal<unknown>;
	seen: Map<number, unknown[]>;
	dispose: Map<number, () => void>;
};

function apply(m: Model, r: Real, o: Op): void {
	switch (o.op) {
		case "edit":
			apply(m, r, { op: "set", value: edit(m.value, o.edit), notify: undefined });
			return;
		case "set": {
			r.signal.set(o.value, o.notify);

			m.base ??= { value: m.value };
			m.value = o.value;
			if (o.notify === false) return;
			if (o.notify) m.force = true;
			if (![...m.listeners.values()].some((l) => !l.pending && !l.gone)) {
				m.base = undefined;
				m.force = false;
			} else if (!m.queue.includes("flush")) {
				m.queue.push("flush");
			}
			return;
		}
		case "listen": {
			const id = m.listeners.size;
			const seen: unknown[] = [];
			r.seen.set(id, seen);
			if (o.kind === "subscribe") {
				r.dispose.set(
					id,
					r.signal.subscribe((v) => seen.push(v)),
				);
			} else if (o.kind === "changed") {
				r.dispose.set(
					id,
					r.signal.changed((v) => seen.push(v)),
				);
			} else {
				const effect = new Effect((effect) => {
					seen.push(effect.get(r.signal));
				});
				r.dispose.set(id, () => effect.close());
				m.queue.push(id);
			}
			m.listeners.set(id, { kind: o.kind, seen: [], pending: o.kind === "effect", gone: false });
			return;
		}
		case "unlisten": {
			if (m.listeners.size === 0) return;
			const id = o.index % m.listeners.size;
			r.dispose.get(id)?.();
			const l = m.listeners.get(id);
			if (l) l.gone = true;
			return;
		}
	}
}

// Drains the model's microtask queue, as the real one drains during a flush.
function settle(m: Model): void {
	for (let task = m.queue.shift(); task !== undefined; task = m.queue.shift()) {
		if (task !== "flush") {
			const effect = m.listeners.get(task);
			if (!effect || effect.gone) continue;
			effect.pending = false;
			effect.seen.push(m.value);
			continue;
		}

		const base = m.base;
		const force = m.force;
		m.base = undefined;
		m.force = false;
		if (!force && (!base || shape(base.value) === shape(m.value))) continue;

		for (const [id, l] of m.listeners) {
			if (l.pending || l.gone) continue;
			if (l.kind === "effect") {
				// An effect reruns on a later microtask, reading the value then.
				l.pending = true;
				m.queue.push(id);
			} else {
				l.seen.push(m.value);
				if (l.kind === "changed") l.gone = true;
			}
		}
	}
}

// One microtask's worth of synchronous operations, then a flush.
class Tick implements fc.AsyncCommand<Model, Real> {
	readonly ops: Op[];
	constructor(ops: Op[]) {
		this.ops = ops;
	}
	check = () => true;
	async run(m: Model, r: Real) {
		for (const o of this.ops) apply(m, r, o);

		for (let i = 0; i < 8; i++) await new Promise<void>((resolve) => queueMicrotask(resolve));
		settle(m);

		for (const [id, l] of m.listeners) {
			const seen = r.seen.get(id) ?? [];
			expect(seen.length, `listener ${id} (${l.kind}) deliveries`).toBe(l.seen.length);
			for (let i = 0; i < seen.length; i++) expect(seen[i]).toBe(l.seen[i]);
		}
	}
	toString() {
		return `tick(${fc.stringify(this.ops)})`;
	}
}

test("signal notifies listeners exactly when the value changes", async () => {
	const ticks = [fc.array(op, { minLength: 1, maxLength: 6 }).map((ops) => new Tick(ops))];

	await fc.assert(
		fc.asyncProperty(value, fc.commands(ticks, { maxCommands: 12 }), async (initial, cmds) => {
			const real: Real = { signal: new Signal(initial), seen: new Map(), dispose: new Map() };
			const model: Model = { value: initial, force: false, listeners: new Map(), queue: [] };
			try {
				await fc.asyncModelRun(() => ({ model, real }), cmds);
			} finally {
				for (const dispose of real.dispose.values()) dispose();
			}
		}),
		{ seed: 0x7369676e, numRuns: 256 },
	);
});
