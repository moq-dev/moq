import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
import usePublishUIContext from "../hooks/use-publish-ui";

export default function NothingSourceButton() {
	const context = usePublishUIContext();
	const onClick = () => {
		context.hangPublish.source.set(undefined);
		context.hangPublish.muted.set(true);
		context.hangPublish.invisible.set(true);
	};

	return (
		<Button
			title="No Source"
			class={`publish-ui__source-button flex--center publish-ui__source-button--no-source ${context.nothingActive() ? "publish-ui__source-button--no-source-active" : ""}`}
			onClick={onClick}
		>
			<Icon.Ban />
		</Button>
	);
}
