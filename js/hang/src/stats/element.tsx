import { render } from "solid-js/web";
import { Effect, Signal } from "@kixelated/signals";
import { StatsWrapper } from "./components/StatsWrapper";
import "./style/index.scss";
import { StatsContext } from "./context";
import type { BroadcastContainer, HandlerProps, StreamContainer } from "./types";

export default class HangStats extends HTMLElement {
	active = new Signal<HangStatsInstance | undefined>(undefined);
	streamSignal = new Signal<HandlerProps | undefined>(undefined);
	#searchEffect: Effect | undefined;

	connectedCallback() {
		this.streamSignal = this.findStreamSource();
		this.active.set(new HangStatsInstance(this));
	}

	disconnectedCallback() {
		this.active.update((prev) => {
			prev?.close();
			return undefined;
		});

		this.#searchEffect?.close();
		this.#searchEffect = undefined;
	}

	private findStreamSource(): Signal<HandlerProps | undefined> {
		const streamSignal = new Signal<HandlerProps | undefined>(undefined);

		this.#searchEffect = new Effect();
		this.#searchEffect.effect(() => {
			let parent = this.parentElement as StreamContainer | null;
			let foundStream: HandlerProps | undefined = undefined;
			
			while (parent) {
				if (parent.active && typeof parent.active.peek === 'function') {
					const instance = parent.active.peek?.();
					
					let streamProps: HandlerProps | undefined;
					if (instance && this.isBroadcastContainer(instance)) {
						streamProps = instance.broadcast;
					} else if (this.isValidStreamSource(instance)) {
						streamProps = instance as HandlerProps;
					}
					
					if (streamProps && this.isValidStreamSource(streamProps)) {
						foundStream = streamProps;
						break;
					}
				}
				parent = parent.parentElement as StreamContainer | null;
			}
			
			streamSignal.set(foundStream);
		});

		return streamSignal;
	}

	private isValidStreamSource(obj: unknown): obj is HandlerProps {
		if (typeof obj !== 'object' || obj === null) return false;
		const source = obj as Record<string, unknown>;
		return 'audio' in source || 'video' in source;
	}

	private isBroadcastContainer(obj: unknown): obj is BroadcastContainer {
		if (typeof obj !== 'object' || obj === null) return false;
		return 'broadcast' in obj;
	}
}

export class HangStatsInstance {
	parent: HangStats;
	#signals: Effect;
	#dispose?: () => void;

	constructor(parent: HangStats) {
		this.parent = parent;
		this.#signals = new Effect();
		this.#signals.effect(this.#render.bind(this));
	}

	close() {
		this.#dispose?.();
	}

	#render(effect: Effect): void {
		const source = effect.get(this.parent.streamSignal);
		const audio = source?.audio;
		const video = source?.video;
		console.log("+++ audio", audio, "video", video);
		if (this.#dispose) {
			this.parent.innerHTML = "";
			this.#dispose();
		}

		const container = document.createElement("div");
		container.className = "stats__container";
		this.#dispose = render(
			() => (
				<StatsContext.Provider value={{ audio, video }}>
					<StatsWrapper />
				</StatsContext.Provider>
			),
			container
		);

		this.parent.appendChild(container);
	}
}