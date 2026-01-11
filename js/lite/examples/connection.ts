import * as Moq from "@moq/lite";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const connection = await Moq.Connection.connect(url);

	console.log("Connected to MoQ relay!");

	// Close the connection when done
	await connection.close();
}

main().catch(console.error);
