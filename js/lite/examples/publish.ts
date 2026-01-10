/**
 * Publishing data example
 *
 * This example demonstrates how to publish data to a MoQ broadcast.
 * Run with: bun run examples/publish.ts
 */
import * as Moq from "../src/index.ts";

async function main() {
	const connection = await Moq.Connection.connect(new URL("https://cdn.moq.dev/anon"));

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

	// Write frames to the group
	group.writeString("Hello, MoQ!");
	group.close();

	// Keep the track open for more data
	await new Promise((resolve) => setTimeout(resolve, 10000));
}

main().catch(console.error);
