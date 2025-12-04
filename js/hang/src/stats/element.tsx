import { render } from "solid-js/web";
import { Effect, Signal } from "@kixelated/signals";
import { StatsWrapper } from "./components/StatsWrapper";
import "./style/index.scss";
import { StatsContext } from "./context";
import type { BroadcastContainer, HandlerProps, StreamContainer } from "./types";

/**
 * hang-stats web component for real-time media streaming metrics
 * Automatically discovers and monitors parent stream sources
 */
export default class HangStats extends HTMLElement {
	/** Signal containing the component instance */
	active = new Signal<HangStatsInstance | undefined>(undefined);
	/** Signal containing discovered stream sources */
	streamSignal = new Signal<HandlerProps | undefined>(undefined);
	/** Effect for managing stream source discovery */
	#searchEffect: Effect | undefined;

	/**
	 * Called when element is inserted into DOM
	 */
	connectedCallback() {
		this.streamSignal = this.findStreamSource();
		this.active.set(new HangStatsInstance(this));
	}

	/**
	 * Called when element is removed from DOM
	 */
	disconnectedCallback() {
		this.active.update((prev) => {
			prev?.close();
			return undefined;
		});

		this.#searchEffect?.close();
		this.#searchEffect = undefined;
	}

	/**
	 * Search DOM tree for parent stream sources
	 * @returns Signal containing discovered stream properties
	 */
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

	/**
	 * Type guard to check if object contains valid stream source properties
	 */
	private isValidStreamSource(obj: unknown): obj is HandlerProps {
		if (typeof obj !== 'object' || obj === null) return false;
		const source = obj as Record<string, unknown>;
		return 'audio' in source || 'video' in source;
	}

	/**
	 * Type guard to check if object has broadcast container properties
	 */
	private isBroadcastContainer(obj: unknown): obj is BroadcastContainer {
		if (typeof obj !== 'object' || obj === null) return false;
		return 'broadcast' in obj;
	}
}

/**
 * Internal class managing component lifecycle and rendering
 */
export class HangStatsInstance {
	/** Reference to parent web component */
	parent: HangStats;
	/** Effect managing reactive updates */
	#signals: Effect;
	/** Disposal function from SolidJS render */
	#dispose?: () => void;

	/**
	 * Initialize component instance
	 * @param parent - Parent HangStats element
	 */
	constructor(parent: HangStats) {
		this.parent = parent;
		this.#signals = new Effect();
		this.#signals.effect(this.#render.bind(this));
	}

	/**
	 * Clean up effects and dispose of rendered content
	 */
	close() {
		this.#dispose?.();
		this.#signals.close();
	}

	/**
	 * Render stats UI with current stream sources
	 * @param effect - The reactive effect
	 */
	#render(effect: Effect): void {
		const source = effect.get(this.parent.streamSignal);
		const audio = source?.audio;
		const video = source?.video;

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