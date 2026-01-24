import { createSignal } from "solid-js";
import Button from "../../shared/components/button/button";
import * as Icon from "../../shared/components/icon/icon";
import usePublishUIContext from "../hooks/use-publish-ui";

export default function FileSourceButton() {
	const [fileInputRef, setFileInputRef] = createSignal<HTMLInputElement | undefined>();
	const context = usePublishUIContext();
	const onClick = () => fileInputRef()?.click();
	const onChange = (event: Event) => {
		const castedInputEl = event.target as HTMLInputElement;
		const file = castedInputEl.files?.[0];

		if (file) {
			context.setFile(file);
			castedInputEl.value = "";
		}
	};

	return (
		<>
			<input
				ref={setFileInputRef}
				onChange={onChange}
				type="file"
				style="display: none;"
				accept="video/*,audio/*,image/*"
			/>
			<Button
				title="Upload File"
				class={`publish-ui__source-button flex--center ${context.fileActive() ? "publish-ui__source-button--active" : ""}`}
				onClick={onClick}
			>
				<Icon.File />
			</Button>
		</>
	);
}
