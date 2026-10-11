import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({ url, consume: origin });

	// Own and close the origin announcement watch.
	const announced = origin.announced();

	// Discover broadcasts announced by the server
	for await (const announcement of announced) {
		if (announcement.kind === "end") continue;
		console.log("New stream available:", announcement.prefix);

		// Subscribe to new streams
		const broadcast = origin.request(announcement.prefix, { announced: true });

		// Read broadcast.active reactively while using it, then release the request.
		broadcast.close();
	}

	announced.close();
	await connection.close();
	origin.close();
}

main().catch(console.error);
