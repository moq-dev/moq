import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const connection = await Moq.Connection.connect(url);

	// Get the announced stream iterator
	const announced = connection.announced();

	// Discover broadcasts announced by the server
	for (;;) {
		const announcement = await announced.next();
		if (!announcement) break;

		console.log("New stream available:", announcement.prefix);

		// Subscribe to new streams
		const _broadcast = connection.consume(announcement.prefix);

		// Do something with the broadcast
	}

	connection.close();
}

main().catch(console.error);
