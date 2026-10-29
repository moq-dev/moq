import "./element.ts";

const carla = document.querySelector("moq-carla");
if (carla) carla.url = import.meta.env.VITE_RELAY_URL ?? "http://localhost:4443";
