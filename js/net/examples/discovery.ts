import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({ url, consume: origin });

	// Get the announced stream iterator
	const announced = connection.announced();

	// Discover broadcasts announced by the server
	for await (const announcement of announced) {
		if (announcement.kind === "retracted") continue;
		console.log("New stream available:", announcement.path);

		// Subscribe to new streams
		const _broadcast = origin.request(announcement.path, { announced: true });

		// Do something with the broadcast
	}

	connection.close();
	origin.close();
}

main().catch(console.error);
