import { createEffect, createSignal } from "solid-js";
import { getHandlerClass } from "../handlers/registry";
import type { Icons, IStatsHandler, HandlerProps } from "../types";

interface StatsItemProps extends HandlerProps {
	icon: Icons;
	svg: string;
}

export const StatsItem = (props: StatsItemProps) => {
	const [displayData, setDisplayData] = createSignal("N/A");
	let handler: IStatsHandler | undefined;

	createEffect(() => {
		handler?.cleanup();

		const HandlerClass = getHandlerClass(props.icon);
		if (!HandlerClass) {
			setDisplayData("N/A");
			return;
		}

		handler = new HandlerClass({
			audio: props.audio,
			video: props.video,
		});

		handler.setup({ setDisplayData });
	});

	return (
		<div class={`stats__item stats__item--${props.icon}`}>
			<div class="stats__icon-wrapper">
				<div class="stats__icon" innerHTML={props.svg} />
			</div>

			<div class="stats__item-detail">
				<span class="stats__item-text">{props.icon}</span>
				<span class="stats__item-data">{displayData()}</span>
			</div>
		</div>
	);
};
