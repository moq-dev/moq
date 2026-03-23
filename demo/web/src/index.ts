import "./highlight";
import "@moq/watch/ui";
import MoqWatch from "@moq/watch/element";
import MoqWatchSupport from "@moq/watch/support/element";
import MoqDiscover from "./discover";

export { MoqDiscover, MoqWatch, MoqWatchSupport };

const watch = document.querySelector("moq-watch") as MoqWatch | undefined;
if (!watch) throw new Error("unable to find <moq-watch> element");

// If query params are provided, use them.
const urlParams = new URLSearchParams(window.location.search);
const name = urlParams.get("broadcast") ?? urlParams.get("name");
const url = urlParams.get("url");

if (url) watch.url = url;
if (name) watch.name = name;
