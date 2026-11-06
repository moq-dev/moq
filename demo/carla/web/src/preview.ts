import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import type { TruckStatus } from "./schemas.ts";
import type { Truck } from "./truck.ts";

/** Disconnected fixtures, loaded only by Vite's development server at ?preview. */
export function populatePreview(add: (path: Moq.Path.Valid) => Truck) {
	const cleanup: (() => void)[] = [];
	for (const [index, pose] of [
		{ x: 30, y: 20, yaw: 0, speed: 5.4 },
		{ x: 35, y: 22, yaw: 10, speed: 3.8 },
		{ x: -40, y: 90, yaw: 90, speed: 0 },
	].entries()) {
		const truck = add(Moq.Path.from(`carla/truck${index + 1}/front.hang`));
		const origin = new Moq.Broadcast.Producer();
		const producer = new Json.Snapshot.Producer<TruckStatus>({
			track: origin.createTrack("status", { compress: true }),
		});
		(truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>).set(origin.consume());
		producer.update({
			value: {
				accepted: { steering: 0, throttle: 0, brake: 1, gear: "neutral" },
				measured: {
					steering: 0.15,
					throttle: index === 2 ? 0 : 0.35,
					brake: index === 2 ? 1 : 0,
					gear: "forward",
				},
				autonomy: "autonomous",
				estopped: false,
				map: "Town10HD_Opt",
				pose,
				media_pipeline: {},
			},
		});
		cleanup.push(() => {
			producer.finish();
			origin.close();
		});
	}
	return () => {
		for (const close of cleanup) close();
	};
}
