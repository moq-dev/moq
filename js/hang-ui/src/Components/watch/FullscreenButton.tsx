import Button from "../shared/components/button";
import type { IconSet } from "../shared/types/icons";
import useWatchUIContext from "./useWatchUIContext";

const fullscreenIcons: IconSet = {
	enter: () => (
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
			aria-hidden="true"
		>
			<path d="M8 3H5a2 2 0 0 0-2 2v3" />
			<path d="M21 8V5a2 2 0 0 0-2-2h-3" />
			<path d="M3 16v3a2 2 0 0 0 2 2h3" />
			<path d="M16 21h3a2 2 0 0 0 2-2v-3" />
		</svg>
	),
	exit: () => (
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
			aria-hidden="true"
		>
			<path d="M8 3v3a2 2 0 0 1-2 2H3" />
			<path d="M21 8h-3a2 2 0 0 1-2-2V3" />
			<path d="M3 16h3a2 2 0 0 1 2 2v3" />
			<path d="M16 21v-3a2 2 0 0 1 2-2h3" />
		</svg>
	),
};

export default function FullscreenButton() {
	const context = useWatchUIContext();

	const onClick = () => {
		context.toggleFullscreen();
	};

	return (
		<Button title="Fullscreen" onClick={onClick}>
			{context.isFullscreen() ? fullscreenIcons.exit() : fullscreenIcons.enter()}
		</Button>
	);
}
