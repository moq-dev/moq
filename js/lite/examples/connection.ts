/**
 * Basic connection example
 *
 * This example demonstrates how to connect to a MoQ relay server.
 * Run with: bun run examples/connection.ts
 */
import * as Moq from "../src/index.ts";

async function main() {
	// Connect to a MoQ relay server
	const connection = await Moq.Connection.connect(new URL("https://cdn.moq.dev/anon"));
	console.log("Connected to MoQ relay!");

	// Close the connection when done
	await connection.close();
}

main().catch(console.error);
