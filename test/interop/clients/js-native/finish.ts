/** Shared finish assertions for native JS and the existing browser client. @module */
import type { Milli } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";

/** Drain a finished media tail, then prove the same one-shot session can still receive. */
export async function readFinish(
	origin: Moq.Origin.Producer,
	path: string,
	frames: number,
	maxDelay: Milli,
): Promise<string> {
	const source = origin.consume();
	const routes = source.announced();
	const request = origin.follow(Moq.Path.from(path));
	const healthRequest = origin.follow(Moq.Path.from(`${path}.health`));
	try {
		let broadcast = request.active.peek();
		while (!broadcast) {
			await request.active.changed();
			broadcast = request.active.peek();
		}
		const catalogs = Catalog.watch(broadcast)[Symbol.asyncIterator]();
		try {
			let catalog = await catalogs.next();
			while (!catalog.done && !Object.keys(catalog.value.video?.renditions ?? {}).length)
				catalog = await catalogs.next();
			if (catalog.done) throw new Error("finished catalog had no video");
			const [name, config] = Object.entries(catalog.value.video?.renditions ?? {})[0];
			const format =
				config.container.kind === "legacy"
					? new Container.Legacy.Format(config)
					: config.container.kind === "loc"
						? new Container.Loc.Format("video")
						: undefined;
			if (!format) throw new Error("unsupported finish media container");
			const subscription = broadcast
				.track(Catalog.track(name, config))
				.subscribe({ maxDelay, groups: { start: { included: 0 } } });
			const media = new Container.Consumer(subscription, { format, maxDelay });
			let count = 0;
			try {
				for (;;) {
					const item = await media.next();
					if (!item) break;
					if (item.kind === "gap") throw new Error("gap in finished tail");
					if (item.kind === "frame" && item.frame.payload.byteLength) count++;
				}
				if ((await subscription.closed) !== null) throw new Error("media did not end cleanly");
				if (count !== frames) throw new Error(`wrong finished tail: ${count} frames, expected ${frames}`);
			} finally {
				media.close();
			}
			while (!(await catalogs.next()).done) {}
			for (;;) {
				const event = await routes.next();
				if (!event) throw new Error("origin ended before broadcast withdrawal");
				if (event.kind === "end" && event.prefix === path) break;
			}
			let health = healthRequest.active.peek();
			while (!health) {
				await healthRequest.active.changed();
				health = healthRequest.active.peek();
			}
			const track = health.track("live").subscribe();
			try {
				const group = await track.recvGroup();
				if (!group) throw new Error("no health group");
				try {
					const frame = await group.readFrame();
					// Its timestamp goes unchecked: lite-05/06 carry an untimed track's send time.
					if (!frame || new TextDecoder().decode(frame.payload) !== "session-alive")
						throw new Error("health frame");
					if ((await group.readFrame()) !== undefined || (await track.recvGroup()) !== undefined)
						throw new Error("health EOF");
				} finally {
					group.close();
				}
				if ((await track.closed) !== null) throw new Error("health did not end cleanly");
			} finally {
				track.close();
			}
			return `finish clean frames=${count} catalog=eof announce=end health=ok`;
		} finally {
			await catalogs.return?.();
		}
	} finally {
		routes.close();
		request.close();
		healthRequest.close();
	}
}
