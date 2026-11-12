// Statechart labels for Handover.tla: what the connection handle reports, what
// the policy holds, and each step as the event that happens.

type Sess = { ep: string; open: boolean; by: string; closing: boolean; goaway: string; age: number };
type Vars = {
	phase: string;
	live: number;
	pred: number;
	probe: string;
	target: string;
	queue: Record<string, string>;
	sess: Record<string, Sess>;
	dial: string;
	dialTo: string;
	lc: string;
	migrating: boolean;
	misdials: number;
};

const at = (v: Vars, s: number) => v.sess[String(s)];

const goaway = (s: Sess) =>
	s.goaway === "-" ? "" : s.goaway === "" ? ", told to restart" : `, told to go to ${s.goaway}`;

const session = (v: Vars, s: number, role: string) => {
	if (s === 0) return null;
	const x = at(v, s);
	return `${role} on ${x.ep}${x.open ? "" : " (closed)"}${goaway(x)}`;
};

const handle = (v: Vars) => {
	switch (v.lc) {
		case "connecting":
			return "Connecting";
		case "live":
			return v.migrating ? "Migrating" : "Connected";
		case "offline":
			return "Disconnected";
		default:
			return "Closed";
	}
};

const policy = (v: Vars) => {
	const doing =
		v.phase === "start"
			? `about to dial ${v.target}`
			: v.phase === "dialing"
				? `dialing ${v.dialTo}${v.dial === "handshake" ? " over the preferred transport" : ""}`
				: v.phase === "backoff"
					? `backing off, next dial ${v.target}`
					: v.phase === "done"
						? "gave up"
						: null;
	const parts = [session(v, v.live, "serving"), session(v, v.pred, "draining"), doing].filter(Boolean);
	return parts.join("; ");
};

export const title = "A client connection across GOAWAY handovers and fallback probes";

export const state = (v: Vars) => ({ "The handle reports": handle(v), "The policy": policy(v) });

const changed = (from: Vars, to: Vars) =>
	Object.keys(to.sess).find((k) => JSON.stringify(from.sess[k]) !== JSON.stringify(to.sess[k]));

export const step = (action: string, from: Vars, to: Vars) => {
	switch (action) {
		case "Pop":
			return `the handle hears ${from.queue["1"] === "Offline" ? "that nothing serves" : from.queue["1"].toLowerCase()}`;
		case "Retire":
			return at(from, from.pred).open
				? "the old session overstays its handover"
				: "the old session's close is noticed";
		case "IdleClose":
			return "the old session, carrying nothing, starts closing";
		case "Dial":
			return `the policy dials ${to.dialTo}`;
		case "Abandon":
			return "the dial times out";
		case "BackoffEnds":
			return "the backoff ends";
		case "LiveEnds": {
			const s = at(from, from.live);
			if (to.phase === "live") return "the policy keeps serving through the restart";
			if (s.goaway !== "-")
				return s.goaway === ""
					? "the policy migrates in place"
					: `the policy follows the GOAWAY to ${s.goaway}`;
			return "the policy sees the session closed";
		}
		case "ProbeAsk":
			return "the policy probes the preferred transport";
		case "ProbeAbandon":
			return "the probe times out";
		case "Connected":
			return `a session to ${from.dialTo} connects`;
		case "Failed":
			return to.phase === "done" ? "the dial fails for good" : "the dial fails";
		case "ProbeFails":
			return "the probe fails";
		case "ProbeGetsThrough":
			return to.phase === "dialing"
				? "the probe gets through, so the fallback drains"
				: "the probe gets through, but a GOAWAY came first";
		case "Peer": {
			const k = changed(from, to);
			if (!k) return null;
			const [a, b] = [from.sess[k], to.sess[k]];
			const who = Number(k) === from.live ? "the live session" : "the old session";
			if (a.goaway !== b.goaway)
				return `the peer tells ${who} ${b.goaway === "" ? "to restart" : `to go to ${b.goaway}`}`;
			if (a.open !== b.open) return `${who} closes`;
			if (a.age !== b.age) return "the live session turns healthy";
			return "the old session stops carrying anything";
		}
		case "Clock":
			return "a deadline passes";
		default:
			return null;
	}
};
