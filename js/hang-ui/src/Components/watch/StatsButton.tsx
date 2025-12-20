import useWatchUIContext from "./useWatchUIContext";

const statsButtonIcon = () => (
	<svg
		xmlns="http://www.w3.org/2000/svg"
		width="16"
		height="16"
		viewBox="0 0 24 24"
		fill="none"
		stroke="currentColor"
		stroke-width="2"
		stroke-linecap="round"
		stroke-linejoin="round"
		class="stats__icon"
	>
		<title>Open statistics</title>
		<path d="M5 21v-6" />
		<path d="M12 21V3" />
		<path d="M19 21V9" />
	</svg>
);

/**
 * Toggle button for showing/hiding stats panel
 */
export default function StatsButton() {
	const context = useWatchUIContext();

	const onClick = () => {
		context.setIsStatsPanelVisible(!context.isStatsPanelVisible());
	};

	return (
		<button
			type="button"
			class="watchControlButton stats__button"
			onClick={onClick}
			title={context.isStatsPanelVisible() ? "Hide stats" : "Show stats"}
			aria-label={context.isStatsPanelVisible() ? "Hide stats" : "Show stats"}
			aria-pressed={context.isStatsPanelVisible()}
		>
			<div class="stats__icon">{statsButtonIcon()}</div>
		</button>
	);
}
