import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const connection = await Moq.Connection.connect(url);

	// Create a broadcast (a collection of tracks)
	const broadcast = new Moq.broadcast.Producer();

	// Insert the "chat" track up front. A subscriber is served directly from this
	// track, no requested() round-trip needed. Mirrors the Rust createTrack/insertTrack.
	void publishTrack(broadcast.createTrack("chat"));

	// Publish the broadcast to the connection
	connection.publish(Moq.Path.from("my-broadcast"), broadcast);
	console.log("Published broadcast: my-broadcast");

	// Tracks created on demand (instead of up front) are still supported: handle any
	// subscribe for a track that wasn't statically inserted.
	for (;;) {
		const request = await broadcast.requested();
		if (!request) break;

		// Reject anything we didn't insert above.
		request.reject(new Error("track not found"));
	}
}

async function publishTrack(track: Moq.track.Producer) {
	console.log("Publishing to track:", track.name);

	// Create a group (e.g., keyframe boundary)
	const group = track.appendGroup();

	// Write two frames to the group
	for (const frame of ["Hello", "MoQ!"]) {
		group.writeString(frame);
	}

	// Mark the group as complete
	group.close();

	// Mark the track as complete (optional)
	track.close();
}

main().catch(console.error);
