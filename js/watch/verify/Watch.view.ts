// Labels for `/statechart` on Watch.tla: the relay connection, the publisher, and what the
// viewer's screen shows, with each step as the viewer or the network makes it happen.

type Vars = {
	net: "connecting" | "connected" | "retrying" | "refused";
	live: boolean;
	active: boolean;
	status: "offline" | "loading" | "live" | "unsupported";
	media: "none" | "playing" | "stalled";
	paused: boolean;
};

const connection = { connecting: "dialing", connected: "connected", retrying: "waiting to redial", refused: "refused" };

const screen = (v: Vars) => {
	if (v.status === "unsupported") return "UPDATE YOUR PLAYER";
	if (v.status === "offline") return "OFFLINE";
	if (v.paused) return "paused";
	if (v.status === "loading" || v.media !== "playing") return "spinner";
	return "playing";
};

export const title = "Watching a broadcast";

export const state = (v: Vars) => ({
	Connection: connection[v.net],
	Publisher: v.live ? "live" : "not live",
	Screen: screen(v),
});

const steps: Record<string, string> = {
	Connect: "the connection opens",
	DialFails: "a dial fails",
	Redial: "the backoff ends",
	Refused: "the relay refuses the token",
	Drop: "the connection drops",
	Start: "the publisher goes live",
	Stop: "the publisher stops",
	Announced: "the broadcast's announcement arrives",
	CatalogArrives: "the catalog arrives",
	CatalogTooNew: "the catalog needs a newer player",
	Frames: "frames arrive",
	Stall: "the video stalls",
	Pause: "you pause",
	Play: "you press play",
};

export const step = (action: string) => steps[action] ?? null;
