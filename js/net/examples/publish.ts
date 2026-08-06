import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");

	// The origin holds what we publish; the connection announces and serves it.
	const origin = new Moq.Origin.Producer();
	await Moq.Connection.connect(url, { publish: origin.consume() });

	// Create a broadcast (a collection of tracks) at a path on the origin
	const broadcast = origin.publish(Moq.Path.from("my-broadcast"));

	// Insert the "chat" track up front. A subscriber is served directly from this
	// track, no requested() round-trip needed. Mirrors the Rust createTrack/insertTrack.
	void publishTrack(broadcast.createTrack("chat"));
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

async function publishTrack(track: Moq.Track.Producer) {
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
