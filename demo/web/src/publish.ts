import "./highlight";
import "@moq/publish/ui";

// We need to import Web Components with fully-qualified paths because of tree-shaking.
import MoqPublish from "@moq/publish/element";
import MoqPublishSupport from "@moq/publish/support/element";

export { MoqPublish, MoqPublishSupport };

const publish = document.querySelector("moq-publish") as MoqPublish;
const watch = document.getElementById("watch") as HTMLAnchorElement;
const watchName = document.getElementById("watch-name") as HTMLSpanElement;

const urlParams = new URLSearchParams(window.location.search);
const name = urlParams.get("broadcast") ?? urlParams.get("name");
if (name) {
	publish.setAttribute("name", name);
	watch.href = `index.html?broadcast=${name}`;
	watchName.textContent = name;
}
