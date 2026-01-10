/**
 * Subscribing to data example
 *
 * This example demonstrates how to subscribe to a MoQ broadcast.
 * Run with: bun run examples/subscribe.ts
 *
 * Note: You'll need to run the publish.ts example in another terminal first.
 */
import * as Moq from "../src/index.ts";

async function main() {
	const connection = await Moq.Connection.connect(new URL("https://cdn.moq.dev/anon"));

	// Subscribe to a broadcast
	const broadcast = connection.consume(Moq.Path.from("my-broadcast"));

	// Subscribe to a specific track
	const track = broadcast.subscribe({ name: "chat" });

	// Read data as it arrives
	for (;;) {
		const group = await track.nextGroup();
		if (!group) break;

		for (;;) {
			const frame = await group.readFrame();
			if (!frame) break;

			console.log("Received:", frame.toString());
		}
	}

	await connection.close();
}

main().catch(console.error);
