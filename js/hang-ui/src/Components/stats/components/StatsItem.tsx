import { createEffect, createSignal, type JSX, onCleanup } from "solid-js";
import { getStatsInformationProvider } from "../providers/registry";
import type { KnownStatsProviders, ProviderProps } from "../types";

/**
 * Props for individual stats metric item
 */
interface StatsItemProps extends ProviderProps {
	name: string;
	/** Metric type identifier */
	statProvider: KnownStatsProviders;
	/** SVG icon markup */
	svg: JSX.Element;
}

/**
 * Individual metric display with provider and reactive updates
 */
export const StatsItem = (props: StatsItemProps) => {
	const [displayData, setDisplayData] = createSignal("N/A");

	createEffect(() => {
		const StatsInformationProvider = getStatsInformationProvider(props.statProvider);

		if (!StatsInformationProvider) {
			setDisplayData("N/A");
			return;
		}

		const provider = new StatsInformationProvider({
			audio: props.audio,
			video: props.video,
		});

		provider.setup({ setDisplayData });

		onCleanup(() => {
			provider.cleanup();
		});
	});

	return (
		<div class={`stats__item stats__item--${props.statProvider}`}>
			<div class="stats__icon-wrapper">
				<div class="stats__icon">{props.svg}</div>
			</div>
			<div class="stats__item-detail">
				<span class="stats__item-title">{props.statProvider}</span>
				<span class="stats__item-data">{displayData()}</span>
			</div>
		</div>
	);
};
