import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
import usePublishUIContext from "../hooks/use-publish-ui";

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
