// Labels for `/statechart` on SubscribeAccounting.tla: what each side holds for the
// group, which update each side is on, and each step as the event that happens.

type Stream = {
	g: number;
	pub: string;
	err: string;
	wasFin: boolean;
	sub: string;
	of: number;
	start: number;
	wrote: number;
	read: number;
};
type Msg = { type: string; g: number; err: string; u: number };
type Vars = {
	cur: Record<string, string>;
	streams: Record<string, Stream>;
	reoffered: number[];
	redrop: number[];
	paused: Record<string, number>;
	resp: Record<string, Msg>;
	sup: number;
	pup: number;
	acct: number[];
	rec: Record<string, string>;
	cached: Record<string, string>;
	got: number[];
	have: Record<string, number>;
};

const list = (seq: Record<string, Stream> | undefined) => Object.values(seq ?? {});

const publisher = (v: Vars) => {
	const last = list(v.streams).at(-1);
	const base = last
		? {
				open: "serving",
				fin: "FIN sent, unacknowledged",
				acked: "served, acknowledged",
				reset: { old: "stream reset as stale", yielded: "stream yielded" }[last.err] ?? "stream reset",
			}[last.pub]
		: { pending: "nothing sent", stale: "named stale", final: "named dropped", skip: "named skipped" }[v.cur["0"]];
	const next = v.paused["0"]
		? ", paused, to continue or drop"
		: v.reoffered.length
			? ", to serve again"
			: v.redrop.length
				? ", to drop again"
				: "";
	return `${base}${next}`;
};

const subscriber = (v: Vars) => {
	const got = v.got.length > 0;
	const base = {
		none: "nothing yet",
		open: "receiving",
		paused: `paused, holding ${v.have["0"]} frames`,
		whole: "has it whole",
		aborted: "reset mid-group",
		evicted: got ? "had it whole, evicted" : "evicted",
	}[v.cached["0"]];
	const rec = { none: "", stale: ", recorded stale", final: ", recorded dropped", skip: ", recorded skipped" }[
		v.rec["0"]
	];
	const owed = v.acct.length ? "" : ", owed";
	return `${base}${rec}${owed}`;
};

export const title = "One lite-07 group: Group Streams, drops, and updates";

export const state = (v: Vars) => ({
	Publisher: publisher(v),
	Subscriber: subscriber(v),
	Updates: v.sup === v.pup ? `on update ${v.sup}` : `update ${v.sup} sent, publisher on ${v.pup}`,
});

export const step = (action: string, from: Vars, to: Vars) => {
	switch (action) {
		case "CursorHand":
			return "the publisher serves the group";
		case "CursorGiveUp":
			return {
				stale: "the publisher gives the group up as stale",
				final: "the publisher drops the group",
				skip: "the publisher skips the group",
			}[to.cur["0"]];
		case "StreamWrite":
			return "the publisher writes a frame";
		case "StreamFin":
			return "the publisher writes the FIN";
		case "StreamYield":
			return "out of stream credit, the publisher resets the stream with YIELDED";
		case "PubContinue":
			return "the publisher continues the paused group on a new stream";
		case "PausedGiveUp":
			return "the publisher gives the paused group up";
		case "StreamFail": {
			const failed = list(to.streams).find((s, i) => s.pub === "reset" && list(from.streams)[i]?.pub !== "reset");
			return failed?.err === "old" ? "the group goes stale and its stream resets" : "the stream fails and resets";
		}
		case "StreamAck":
			return "the publisher's FIN is acknowledged";
		case "PubOutcomes":
			return "the publisher names the reset stream in a drop";
		case "PubRedrop":
			return "the publisher drops it again";
		case "PubReoffer":
			return "the publisher serves it again";
		case "PubUpdate":
			return "the update reaches the publisher";
		case "PubFold":
			return "the publisher's record folds the group under the pool's debt";
		case "TailFold":
			return "the subscriber's tail forgets the group finished, under the pool's debt";
		case "SubHeader":
			return "a Group Stream's header arrives";
		case "SubFrame":
			return "the subscriber reads a frame";
		case "SubFin":
			return "the subscriber reads the stream to its FIN";
		case "SubReset": {
			const ended = list(to.streams).find((s, i) => s.sub === "reset" && list(from.streams)[i]?.sub !== "reset");
			return ended?.err === "yielded" ? "the stream's YIELDED reset arrives" : "the stream's reset arrives";
		}
		case "SubResetHeld":
			return "the stream's reset arrives";
		case "Evict":
			return "the subscriber's cache evicts the group";
		case "SubRecv": {
			const msg = from.resp["1"];
			if (msg.type === "skip") return "a skip arrives";
			return msg.err === "old" ? `a stale drop under update ${msg.u} arrives` : "a drop arrives";
		}
		case "SubUpdate":
			return "the subscriber sends an update";
		default:
			return null;
	}
};
