// Statechart labels for RouteHandoff.tla: each relay's routes and what its
// reader shows, and each step as the message that arrives.

// A sequence, such as a path, is keyed from 1; a hop is <<node, peer, generation>>.
type Hop = Record<string, number>;
type Path = Record<string, Hop>;
type Route = { on: boolean; held: boolean; next: number; path: Path };
type Node = {
	rt: Record<string, Route>;
	doubt: number[];
	shown: Record<string, { item: number; avail: boolean }>;
};
type Msg = { t: string; k: string; path: Path; hop: Hop; gone: boolean };
type Vars = {
	st: Record<string, Node>;
	q: Record<string, Record<string, Msg>>;
	up: number[][];
	burst: number;
	churn: number;
};

const ORIGIN = "0";
const name = (n: string | number) => (String(n) === ORIGIN ? "the publisher" : `relay ${n}`);
const hops = (path: Path) => Object.keys(path).length - 1;

const route = (key: string, r: Route) => {
	if (!r.on) return null;
	if (r.held) return `${key} held${r.next >= 0 ? ` under ${name(r.next)}` : ""}`;
	const n = hops(r.path);
	return `${key} via ${name(r.next)}, ${n} hop${n === 1 ? "" : "s"}`;
};

const relay = (s: Node) => {
	const routes = Object.entries(s.rt)
		.map(([key, r]) => route(key === "root" ? "root" : `"${key}"`, r))
		.filter((r) => r !== null);
	const shows = Object.entries(s.shown)
		.filter(([, x]) => x.item >= 0)
		.map(([c, x]) => `${c}${x.avail ? "" : " (unavailable)"}`);
	const doubt = s.doubt.length ? `, doubts ${s.doubt.map(name).join(" and ")}` : "";
	return `${routes.join("; ") || "no route"}${shows.length ? `, shows ${shows.join(", ")}` : ""}${doubt}`;
};

const sessions = (v: Vars) =>
	v.up
		.map((l) => [...l].sort((a, b) => a - b).join("–"))
		.sort()
		.join(", ") || "none";

export const title = "One publisher's routes across a mesh of relays";

export const state = (v: Vars) => ({
	"Open sessions": sessions(v),
	...Object.fromEntries(
		Object.keys(v.st)
			.filter((n) => n !== ORIGIN)
			.map((n) => [`Relay ${n}`, relay(v.st[n])]),
	),
	"Messages this burst": `${v.burst} for ${v.churn} change${v.churn === 1 ? "" : "s"}`,
});

const len = (s: Record<string, unknown> | undefined) => (s ? Object.keys(s).length : 0);

// A queue key `<<m, n>>` is a tuple, keyed from 1.
const ends = (key: string | undefined) => {
	const tuple = key ? JSON.parse(key) : {};
	return [tuple["1"] ?? "?", tuple["2"] ?? "?"];
};

export const step = (action: string, from: Vars, to: Vars) => {
	switch (action) {
		case "Announce":
			return "the publisher announces a claim";
		case "Unannounce":
			return "the publisher withdraws a claim";
		case "Connect":
		case "Disconnect": {
			const before = new Set(from.up.map((l) => [...l].sort().join("–")));
			const after = new Set(to.up.map((l) => [...l].sort().join("–")));
			const changed = [...before, ...after].find((l) => before.has(l) !== after.has(l));
			return `the ${changed} session ${action === "Connect" ? "opens" : "closes"}`;
		}
		case "Lift": {
			const n = Object.keys(from.st).find((k) => len(to.st[k].doubt) < len(from.st[k].doubt));
			return n ? `${name(n)}'s timer fires and its doubt lifts` : "a timer fires";
		}
		case "Deliver": {
			const d = Object.keys(from.q).find((k) => len(to.q[k]) === len(from.q[k]) - 1);
			const [m, n] = ends(d);
			const msg = from.q[d ?? ""]?.["1"];
			if (!msg) return "a message arrives";
			const key = msg.k === "root" ? "root" : `"${msg.k}"`;
			const what =
				msg.t === "dead"
					? `news that ${name(msg.hop["1"])}'s session to ${name(msg.hop["2"])} closed${msg.gone ? ", and its routes through it end" : ""}`
					: msg.t === "offer"
						? `${key} route (${hops(msg.path)} hop${hops(msg.path) === 1 ? "" : "s"})`
						: msg.t === "claims"
							? `${key} claims`
							: `${key} route ${msg.t}`;
			return `${name(m)}'s ${what} reaches ${name(n)}`;
		}
		default:
			return null;
	}
};
