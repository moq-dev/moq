// Labels for `/statechart` on RouteHandoff.tla: each relay's route and what its
// reader shows, and each step as the message that arrives.

// A sequence such as `hops` is keyed from 1.
type Entry = { on: boolean; seq: number; met: number; held: boolean; hops: Record<string, number> };
type Node = {
	rx: Record<string, Entry>;
	fd: { on: boolean; seq: number; met: number };
	shown: Record<string, { item: number; avail: boolean }>;
};
type Msg = { type: string; reach: { kind: string; seq: number; met: number; hops: Record<string, number> } };
type Vars = {
	st: Record<string, Node>;
	q: Record<string, Record<string, Msg>>;
	acks: Record<string, number>;
	up: number[][];
	own: Record<string, number>;
};

const ORIGIN = "0";
const name = (n: string | number) => (String(n) === ORIGIN ? "the origin" : `relay ${n}`);

const hops = (e: { hops: Record<string, number> }) => Object.keys(e.hops).length;

// Never a route that crossed node `n`, then Babel's condition.
const feasible = (n: string, s: Node, e: Entry) =>
	!Object.values(e.hops).some((hop) => String(hop) === n) &&
	(!s.fd.on || e.seq > s.fd.seq || (e.seq === s.fd.seq && e.met < s.fd.met));

// `selected` where every link costs the same: the usable feasible route with the
// lowest metric, then key, and the hops to the origin through it.
const selected = (n: string, s: Node) => {
	let best: [string, number, number] | null = null;
	for (const [m, e] of Object.entries(s.rx)) {
		if (!e.on || e.held || !feasible(n, s, e)) continue;
		if (best === null || e.met < best[2]) best = [m, hops(e), e.met];
	}
	return best;
};

const relay = (n: string, s: Node) => {
	const route = selected(n, s);
	const shows = Object.values(s.shown).some((x) => x.item >= 0);
	const held = Object.values(s.rx).some((e) => e.on);
	const how = route
		? `via ${route[0] === ORIGIN ? "the origin" : `relay ${route[0]}`}, ${route[1]} hop${route[1] === 1 ? "" : "s"}`
		: held
			? "holding"
			: s.fd.on
				? "no route, distance kept"
				: "no route";
	return `${how}${shows ? ", shows it" : ""}`;
};

const sessions = (v: Vars) => {
	const down = ["0,1", "0,2", "1,2"].filter((l) => !v.up.some((u) => [...u].sort().join(",") === l));
	return down.length === 0 ? "all open" : down.map((l) => `${l.replace(",", "–")} closed`).join(", ");
};

export const title = "One origin identity across a triangle of relays";

export const state = (v: Vars) => ({
	Sessions: sessions(v),
	"Relay 1": relay("1", v.st["1"]),
	"Relay 2": relay("2", v.st["2"]),
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
			return "the origin announces the broadcast";
		case "Unannounce":
			return "the origin withdraws the broadcast";
		case "Connect":
		case "Disconnect": {
			const before = new Set(from.up.map((l) => [...l].sort().join("–")));
			const after = new Set(to.up.map((l) => [...l].sort().join("–")));
			const changed = [...before, ...after].find((l) => before.has(l) !== after.has(l));
			return `the ${changed} session ${action === "Connect" ? "opens" : "closes"}`;
		}
		case "Collect":
			return "a relay with no route collects its state";
		case "DeliverAck": {
			const [m, n] = ends(Object.keys(from.acks).find((k) => to.acks[k] < from.acks[k]));
			return `${name(m)}'s echo of a route end reaches ${name(n)}`;
		}
		case "Deliver": {
			const d = Object.keys(from.q).find((k) => len(to.q[k]) === len(from.q[k]) - 1);
			const [m, n] = ends(d);
			const msg = from.q[d ?? ""]?.["1"];
			if (!msg) return "a message arrives";
			if (msg.type === "batch") return `${name(m)}'s claims reach ${name(n)}`;
			const what =
				msg.type === "request"
					? "seqno request"
					: msg.reach.kind === "none"
						? "route end"
						: msg.reach.kind === "held"
							? "hold"
							: String(m) === ORIGIN
								? "route"
								: `route (${hops(msg.reach) - 1} hop${hops(msg.reach) === 2 ? "" : "s"})`;
			return `${name(m)}'s ${what} reaches ${name(n)}`;
		}
		default:
			return null;
	}
};
