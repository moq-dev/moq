/**
 * Stream discovery example
 *
 * This example demonstrates how to discover broadcasts announced by the server.
 * Run with: bun run examples/discovery.ts
 */
import * as Moq from "../src/index.ts";

async function main() {
	const connection = await Moq.Connection.connect(new URL("https://cdn.moq.dev/anon"));

	// Get the announced stream iterator
	const announced = connection.announced();

	// Discover broadcasts announced by the server
	for (;;) {
		const announcement = await announced.next();
		if (!announcement) break;

		console.log("New stream available:", announcement.path);

		// Subscribe to new streams
		connection.consume(announcement.path);
		// ... handle the broadcast
	}

	await connection.close();
}

main().catch(console.error);
