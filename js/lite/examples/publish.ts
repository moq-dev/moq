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
		const track = await broadcast.requested();
		if (!track) break;

		// Accept the request for the "chat" track
		if (track.name === "chat") {
			publishTrack(track);
		} else {
			// Reject other tracks
			track.close(new Error("track not found"));
		}
	}
}

async function publishTrack(track: Moq.Track) {
	console.log("Publishing to track:", track.name);

	// Create a group (e.g., keyframe boundary)
	const group = track.appendGroup();

	// Write frames to the group
	const frame = Moq.Frame.fromString("Hello, MoQ!");
	group.writeFrame(frame);

	// Mark the group as complete
	group.close();

	// Mark the track as complete (optional)
	track.close();
}

main().catch(console.error);
