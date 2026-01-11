import Button from "../shared/button";
import * as Icon from "../shared/icon";
import usePublishUIContext from "./usePublishUIContext";

export default function ScreenSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		context.hangPublish.source.set("screen");
		context.hangPublish.invisible.set(false);
		context.hangPublish.muted.set(false);
	};

	return (
		<div class="publishSourceButtonContainer">
			<Button
				title="Screen"
				class={`publishSourceButton ${context.screenActive() ? "active" : ""}`}
				onClick={onClick}
			>
				<Icon.Screen />
			</Button>
		</div>
	);
}
