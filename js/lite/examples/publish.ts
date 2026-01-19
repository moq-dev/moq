import * as Moq from "@moq/lite";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const connection = await Moq.Connection.connect(url);

	// Create a broadcast (a collection of tracks)
	const broadcast = new Moq.Broadcast();

	// Publish the broadcast to the connection
	connection.publish(Moq.Path.from("my-broadcast"), broadcast);
	console.log("Published broadcast: my-broadcast");

	// Wait for subscription requests
	for (;;) {
		const request = await broadcast.requested();
		if (!request) break;

		// Accept the request for the "chat" track
		if (request.track.name === "chat") {
			publishTrack(request.track);
		} else {
			// Reject other tracks
			request.track.close(new Error("track not found"));
		}
	}
}

async function publishTrack(track: Moq.Track) {
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
