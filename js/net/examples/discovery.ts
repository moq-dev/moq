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

		const prefix = announcement.pattern.asPrefix();
		if (prefix === undefined) continue;
		console.log("New stream available:", prefix);

		// Subscribe to new streams
		const _broadcast = connection.consume(Moq.Path.from(prefix));

		// Do something with the broadcast
	}

	connection.close();
}

main().catch(console.error);
