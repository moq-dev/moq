// Labels for `/statechart` on Failover.tla: how many relays are up, where the viewer's connection
// is, whether the relays reach the origin, and what the viewer's screen shows. The two relays are
// alike, so labels name them by role ("your relay", "the other relay") rather than by number.

type Vars = {
	up: number[];
	on: number;
	loop: "serving" | "migrating" | "dialing" | "waiting" | "stopped";
	pin: number;
	path: "ok" | "lost";
	handle: boolean;
	media: "playing" | "waiting";
};

const relays = (up: number[]) => ["none up", "one up", "both up"][up.length];

const connection = (v: Vars) => {
	switch (v.loop) {
		case "serving":
			return v.pin ? "on the relay it was sent to" : "on a relay";
		case "migrating":
			return "on a leaving relay, dialing another";
		case "stopped":
			return "stopped";
		default:
			return v.pin ? "redialing only the relay it was sent to" : "redialing";
	}
};

const screen = (v: Vars) => (!v.handle ? "OFFLINE" : v.media === "waiting" ? "spinner" : "playing");

export const title = "Relay failover, from the viewer's side";

export const state = (v: Vars) => ({
	Relays: relays(v.up),
	Connection: connection(v),
	Origin: v.path === "ok" ? "reachable" : "unreachable",
	Screen: screen(v),
});

export const step = (action: string, from: Vars, to: Vars) => {
	const gone = from.up.find((r) => !to.up.includes(r));
	switch (action) {
		case "GoawayRestart":
			return "your relay restarts and sends GOAWAY";
		case "GoawayTo":
			return "your relay sends you to the other relay";
		case "GoawayRefused":
			return "your relay sends you to another host";
		case "Replaced":
			return "the replacement connects";
		case "HandoverEnds":
			return "the handover ends first";
		case "Crash":
			return gone === from.on ? "your relay crashes" : "the other relay crashes";
		case "Recover":
			return "a relay comes back";
		case "BackoffEnds":
			return null;
		case "Connected":
			return "the redial connects";
		case "DialFails":
			return "the redial fails";
		case "Announced":
			return "the broadcast's announcement arrives";
		case "Frames":
			return "frames arrive";
		case "Reroute":
			return "the relays move to another path to the origin";
		case "PathLost":
			return "the relays lose the origin";
		case "PathBack":
			return "the relays reach the origin again";
		default:
			return null;
	}
};
