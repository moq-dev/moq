import * as Moq from "@moq/net";

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const origin = new Moq.Origin.Producer();
	const connection = await Moq.Connection.connect({ url, consume: origin });

	// Own and close the origin announcement watch.
	const announced = origin.announced();
	const requests = new Map<Moq.Path.Valid, Moq.Origin.Requesting>();

	// Discover broadcasts announced by the server
	for await (const announcement of announced) {
		if (announcement.kind === "end" || requests.has(announcement.prefix)) continue;
		console.log("New stream available:", announcement.prefix);

		// One request per path follows its publisher restarts until closed.
		const request = origin.follow(announcement.prefix);
		request.active.subscribe((broadcast) => console.log(request.path, broadcast ? "live" : "offline"));
		requests.set(announcement.prefix, request);
	}

	for (const request of requests.values()) request.close();
	announced.close();
	await connection.close();
	origin.close();
}

main().catch(console.error);
