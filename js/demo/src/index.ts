import "./highlight";
import "@moq/watch/ui";
import MoqWatch from "@moq/watch/element";
import MoqWatchSupport from "@moq/watch/support/element";
import MoqWatchConfig from "./config";

export { MoqWatchSupport, MoqWatch, MoqWatchConfig };

const watch = document.querySelector("moq-watch") as MoqWatch | undefined;
const config = document.querySelector("moq-watch-config") as MoqWatchConfig | undefined;

if (!watch) throw new Error("unable to find <moq-watch> element");

// If query params are provided, use them.
const urlParams = new URLSearchParams(window.location.search);
const name = urlParams.get("broadcast") ?? urlParams.get("name");
const url = urlParams.get("url");

if (name) {
	watch.setAttribute("name", name);
	config?.setAttribute("name", name);
}
if (url) {
	watch.setAttribute("url", url);
	config?.setAttribute("url", url);
}

// Sync config changes to the watch element.
config?.addEventListener("change", (e) => {
	const { url, name } = (e as CustomEvent).detail;
	if (url) watch.setAttribute("url", url);
	if (name) watch.setAttribute("name", name);
});
