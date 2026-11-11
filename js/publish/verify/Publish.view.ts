// Labels for `/statechart` on Publish.tla: what the publisher's badge says, and whether viewers
// can find the broadcast, with each step as the publisher or the network makes it happen.

type Vars = {
	camera: "off" | "asking" | "capturing" | "denied" | "lost" | "failed";
	settled: boolean;
	announced: boolean;
	net: "connecting" | "connected";
};

const badge = (v: Vars) => {
	if (v.camera === "denied") return "CAMERA DENIED";
	if (v.camera === "failed") return "CAMERA FAILED";
	if (v.net === "connecting") return "CONNECTING";
	if (v.camera === "capturing") return "LIVE";
	return "NO SOURCE";
};

export const title = "Publishing a camera broadcast";

export const state = (v: Vars) =>
	`${badge(v)}, viewers ${v.announced && v.net === "connected" ? "can" : "cannot"} find it`;

const steps: Record<string, string> = {
	Choose: "you pick the camera",
	Grant: "you allow the camera",
	Deny: "you deny the camera",
	Settle: "the encoders settle",
	Lost: "the camera is lost",
	Reopened: "the camera reopens",
	Spent: "the reopen budget runs out",
	Connect: "the connection opens",
	Drop: "the connection drops",
};

// Hides "you stop the camera", which every selected state has and which leads back to the start:
// StopStopsPublishing checks it.
export const step = (action: string) => steps[action] ?? null;
