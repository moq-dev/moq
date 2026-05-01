import { createAccessor } from "@moq/signals/solid";
import { createMemo, For, onCleanup, Show } from "solid-js";
import { Detection } from "../../detection";
import useWatchUIContext from "../hooks/use-watch-ui";

export default function BoundingBoxes() {
	const context = useWatchUIContext();
	const broadcast = context.moqWatch.broadcast;

	// Subscribe to the detection track whenever the catalog advertises one.
	const detection = new Detection(broadcast.active, broadcast.catalog, {
		enabled: true,
	});
	onCleanup(() => detection.close());

	const latest = createAccessor(detection.latest);

	const boxes = createMemo(() => latest()?.boxes ?? []);

	return (
		<Show when={boxes().length > 0}>
			<svg class="watch-ui__detections" viewBox="0 0 1 1" preserveAspectRatio="none" aria-hidden="true">
				<For each={boxes()}>
					{(box) => (
						<g>
							<rect x={box.x} y={box.y} width={box.w} height={box.h} class="watch-ui__detections-box" />
							<Show when={box.label}>
								{(label) => (
									<text
										x={box.x}
										y={box.y}
										class="watch-ui__detections-label"
										dominant-baseline="text-bottom"
									>
										{label()}
										<Show when={box.score}>{(score) => ` ${(score() * 100).toFixed(0)}%`}</Show>
									</text>
								)}
							</Show>
						</g>
					)}
				</For>
			</svg>
		</Show>
	);
}
