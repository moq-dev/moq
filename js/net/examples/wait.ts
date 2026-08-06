import * as Moq from "@moq/net";

// @moq/net re-exports the reactive primitives, so an app only needs the one dependency.
const { Effect } = Moq.Signals;

async function main() {
	const url = new URL("https://cdn.moq.dev/anon");
	const connection = new Moq.Connection.Reload({ url, enabled: true });

	// Wait for a broadcast that may not exist yet. `consume` would subscribe blind and get reset
	// if nobody is publishing the path; this waits for the announcement instead.
	const broadcast = connection.announcedBroadcast(Moq.Path.from("my-broadcast"));

	const effect = new Effect();
	effect.run((effect) => {
		// Re-runs every time the broadcast comes online or goes away, including across reconnects
		// and same-name republishes.
		const active = effect.get(broadcast.active);
		if (!active) {
			console.log("broadcast is offline");
			return;
		}

		console.log("broadcast is live");
		const track = active.track("chat").subscribe({ priority: 0 });
		effect.cleanup(() => track.close());

		effect.spawn(async () => {
			for (;;) {
				const group = await Promise.race([effect.cancel, track.recvGroup()]);
				if (!group) break;
				console.log("received:", await group.readString());
			}
		});
	});

	// Run until interrupted. `closed` rejects if the reconnect loop gives up, so release
	// everything on the way out either way.
	try {
		await connection.closed;
	} finally {
		effect.close();
		broadcast.close();
		connection.close();
	}
}

main().catch(console.error);
