import { For, type JSX } from "solid-js";
import type { KnownStatsProviders, ProviderProps } from "../types";
import { StatsItem } from "./StatsItem";

/**
 * Props for stats panel component
 */
interface StatsPanelProps extends ProviderProps {}

export const statsDetailItems: { name: string; statProvider: KnownStatsProviders; svg: () => JSX.Element }[] = [
	{
		name: "Network",
		statProvider: "network",
		svg: () => (
			<svg
				xmlns="http://www.w3.org/2000/svg"
				width="24"
				height="24"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
				class="stats__icon"
			>
				<title>Network statistics</title>
				<path d="M16.247 7.761a6 6 0 0 1 0 8.478" />
				<path d="M19.075 4.933a10 10 0 0 1 0 14.134" />
				<path d="M4.925 19.067a10 10 0 0 1 0-14.134" />
				<path d="M7.753 16.239a6 6 0 0 1 0-8.478" />
				<circle cx="12" cy="12" r="2" />
			</svg>
		),
	},
	{
		name: "Video",
		statProvider: "video",
		svg: () => (
			<svg
				xmlns="http://www.w3.org/2000/svg"
				width="24"
				height="24"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
				class="stats__icon"
			>
				<title>Video statistics</title>
				<path d="m16 13 5.223 3.482a.5.5 0 0 0 .777-.416V7.87a.5.5 0 0 0-.752-.432L16 10.5" />
				<rect x="2" y="6" width="14" height="12" rx="2" />
			</svg>
		),
	},
	{
		name: "Audio",
		statProvider: "audio",
		svg: () => (
			<svg
				xmlns="http://www.w3.org/2000/svg"
				width="24"
				height="24"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
				class="stats__icon"
			>
				<title>Audio statistics</title>
				<path d="M9 18V5l12-2v13" />
				<circle cx="6" cy="18" r="3" />
				<circle cx="18" cy="16" r="3" />
			</svg>
		),
	},
	{
		name: "Buffer",
		statProvider: "buffer",
		svg: () => (
			<svg
				xmlns="http://www.w3.org/2000/svg"
				width="24"
				height="24"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
				class="stats__icon"
			>
				<title>Buffer statistics</title>
				<path d="M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2" />
			</svg>
		),
	},
];

/**
 * Panel displaying all metrics in a grid layout
 */
export const StatsPanel = (props: StatsPanelProps) => {
	return (
		<div class="stats__panel">
			<For each={statsDetailItems}>
				{({ name, statProvider, svg }) => (
					<StatsItem
						name={name}
						statProvider={statProvider}
						svg={svg()}
						audio={props.audio}
						video={props.video}
					/>
				)}
			</For>
		</div>
	);
};
