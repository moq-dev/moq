import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");

	// The origin holds what we publish; the connection announces and serves it.
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({ url, publish: origin.consume() });

	// Route a path on the origin. The route owns a broadcast (a collection of tracks) and announces it.
	const route = origin.route(Moq.Path.from("my-broadcast"));
	const broadcast = route.broadcast;

	// Create the "chat" track up front. A subscriber is served directly from this
	// track, no requested() round-trip needed. Mirrors Rust's create_track.
	// Each frame is stamped with the time it was written, so the track declares a timescale.
	// Undefined only while another track holds the name, which a fresh broadcast never has.
	const track = broadcast.createTrack("chat", { timescale: Moq.Time.Timescale.MILLI });
	if (!track) throw new Error("the chat track already exists");
	void publishTrack(track);
	console.log("Published broadcast: my-broadcast");

	await connection.closed;
}

async function publishTrack(track: Moq.Track.Producer) {
	console.log("Publishing to track:", track.name);

	// Create a group (e.g., keyframe boundary) from its first frame, which is where it starts
	const encoder = new TextEncoder();
	const group = track.appendGroup({ payload: encoder.encode("Hello"), timestamp: Moq.Time.Timestamp.now() });

	// Write the rest of the group's frames
	group.writeFrame({ payload: encoder.encode("MoQ!"), timestamp: Moq.Time.Timestamp.now() });

	// Mark the group as complete
	group.finish();

	// Mark the track as complete (optional)
	track.finish();
	track.close();
}

main().catch(console.error);
