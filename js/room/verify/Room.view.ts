// Labels for `/statechart` on Room.tla: what you publish, your connection, what Bob publishes, and
// what of Bob is on your screen.

type Vars = {
	joined: boolean;
	sharing: boolean;
	conn: "up" | "down";
	bob: string[];
	seen: string[];
};

const broadcasts = (kinds: string[], none: string) => {
	const camera = kinds.includes("camera");
	const screen = kinds.includes("screen");
	if (camera && screen) return "camera and screen";
	if (camera) return "camera";
	if (screen) return "screen only";
	return none;
};

const you = (v: Vars) => {
	if (v.joined) return v.sharing ? "joined, sharing" : "joined";
	return v.sharing ? "not joined, sharing" : "not joined";
};

export const title = "Joining and leaving a room";

export const state = (v: Vars) => ({
	You: you(v),
	Connection: v.conn === "up" ? "connected" : "reconnecting",
	Bob: broadcasts(v.bob, "not in the room"),
	"Bob on your screen": broadcasts(v.seen, "absent"),
});

export const step = (action: string, from: Vars, to: Vars) => {
	const started = to.bob.find((k) => !from.bob.includes(k));
	const ended = from.bob.find((k) => !to.bob.includes(k));
	const shown = to.seen.find((k) => !from.seen.includes(k));
	const hidden = from.seen.find((k) => !to.seen.includes(k));
	switch (action) {
		case "Join":
			return "you join";
		case "Leave":
			return "you leave";
		case "Share":
			return "you share your screen";
		case "StopSharing":
			return "your share ends";
		case "Drop":
			return "your connection drops";
		case "Reconnect":
			return "you reconnect";
		case "BobStarts":
			return started === "camera" ? "Bob joins" : "Bob shares his screen";
		case "BobEnds":
			return ended === "camera" ? "Bob leaves" : "Bob's share ends";
		case "SeeStart":
			return `Bob's ${shown} announcement arrives`;
		case "SeeEnd":
			return `Bob's ${hidden} end arrives`;
		default:
			return null;
	}
};
