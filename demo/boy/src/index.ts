import "@moq/boy/element";
import "@moq/boy/ui";

const url = import.meta.env.VITE_RELAY_URL || "http://localhost:4443/anon";

const boy = document.querySelector("moq-boy");
if (boy) boy.url = url;
