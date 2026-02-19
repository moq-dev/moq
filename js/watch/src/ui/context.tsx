import { type Moq, Signals } from "@moq/hang";
import solid from "@moq/signals/solid";
import type { JSX } from "solid-js";
import { createContext, createSignal, onCleanup } from "solid-js";
import type { BufferedRanges } from "..";
import type MoqWatch from "../element";

type WatchUIContextProviderProps = {
	moqWatch: MoqWatch;
	children: JSX.Element;
};

type WatchStatus = "no-url" | "disconnected" | "connecting" | "offline" | "loading" | "live" | "connected";

export type Rendition = {
	name: string;
	width?: number;
	height?: number;
};

export type WatchUIContextValues = {
	moqWatch: MoqWatch;
	watchStatus: () => WatchStatus;
	isPlaying: () => boolean;
	isMuted: () => boolean;
	setVolume: (vol: number) => void;
	currentVolume: () => number;
	togglePlayback: () => void;
	toggleMuted: () => void;
	buffering: () => boolean;
	jitter: () => Moq.Time.Milli;
	setJitter: (value: Moq.Time.Milli) => void;
	availableRenditions: () => Rendition[];
	activeRendition: () => string | undefined;
	setActiveRendition: (name: string | undefined) => void;
	isStatsPanelVisible: () => boolean;
	setIsStatsPanelVisible: (visible: boolean) => void;
	isFullscreen: () => boolean;
	toggleFullscreen: () => void;
	timestamp: () => Moq.Time.Milli | undefined;
	videoBuffered: () => BufferedRanges;
	audioBuffered: () => BufferedRanges;
};

export const WatchUIContext = createContext<WatchUIContextValues>();

export default function WatchUIContextProvider(props: WatchUIContextProviderProps) {
	const [watchStatus, setWatchStatus] = createSignal<WatchStatus>("no-url");
	const [isPlaying, setIsPlaying] = createSignal<boolean>(false);
	const [isMuted, setIsMuted] = createSignal<boolean>(false);
	const [currentVolume, setCurrentVolume] = createSignal<number>(0);
	const [buffering, setBuffering] = createSignal<boolean>(false);
	const jitter = solid(props.moqWatch.jitter);
	const [availableRenditions, setAvailableRenditions] = createSignal<Rendition[]>([]);
	const [activeRendition, setActiveRendition] = createSignal<string | undefined>(undefined);
	const [isStatsPanelVisible, setIsStatsPanelVisible] = createSignal<boolean>(false);
	const [isFullscreen, setIsFullscreen] = createSignal<boolean>(!!document.fullscreenElement);

	const togglePlayback = () => {
		props.moqWatch.paused.set(!props.moqWatch.paused.get());
	};

	const toggleFullscreen = () => {
		if (document.fullscreenElement) {
			document.exitFullscreen();
		} else {
			props.moqWatch.requestFullscreen();
		}
	};

	const setVolume = (volume: number) => {
		props.moqWatch.audio.volume.set(volume / 100);
	};

	const toggleMuted = () => {
		props.moqWatch.audio.muted.update((muted) => !muted);
	};

	const setJitter = (latency: Moq.Time.Milli) => {
		props.moqWatch.jitter.set(latency);
	};

	const setActiveRenditionValue = (name: string | undefined) => {
		props.moqWatch.video.source.target.update((prev) => ({
			...prev,
			name: name,
		}));
	};

	// Use solid helper for the new signals
	const timestamp = solid(props.moqWatch.video.timestamp);
	const videoBuffered = solid(props.moqWatch.video.buffered);
	const audioBuffered = solid(props.moqWatch.audio.buffered);

	const value: WatchUIContextValues = {
		moqWatch: props.moqWatch,
		watchStatus,
		togglePlayback,
		isPlaying,
		setVolume,
		isMuted,
		currentVolume,
		toggleMuted,
		buffering,
		jitter,
		setJitter,
		availableRenditions,
		activeRendition,
		setActiveRendition: setActiveRenditionValue,
		isStatsPanelVisible,
		setIsStatsPanelVisible,
		isFullscreen,
		toggleFullscreen,
		timestamp,
		videoBuffered,
		audioBuffered,
	};

	const watch = props.moqWatch;
	const signals = new Signals.Effect();

	signals.run((effect) => {
		const url = effect.get(watch.connection.url);
		const connection = effect.get(watch.connection.status);
		const broadcast = effect.get(watch.broadcast.status);

		if (!url) {
			setWatchStatus("no-url");
		} else if (connection === "disconnected") {
			setWatchStatus("disconnected");
		} else if (connection === "connecting") {
			setWatchStatus("connecting");
		} else if (broadcast === "offline") {
			setWatchStatus("offline");
		} else if (broadcast === "loading") {
			setWatchStatus("loading");
		} else if (broadcast === "live") {
			setWatchStatus("live");
		} else if (connection === "connected") {
			setWatchStatus("connected");
		}
	});

	signals.run((effect) => {
		const paused = effect.get(watch.paused);
		setIsPlaying(!paused);
	});

	signals.run((effect) => {
		const volume = effect.get(watch.audio.volume);
		setCurrentVolume(volume * 100);
	});

	signals.run((effect) => {
		const muted = effect.get(watch.audio.muted);
		setIsMuted(muted);
	});

	signals.run((effect) => {
		const stalled = effect.get(watch.video.stalled);
		setBuffering(stalled);
	});

	signals.run((effect) => {
		const jitter = effect.get(watch.jitter);
		setJitter(jitter);
	});

	signals.run((effect) => {
		const videoCatalog = effect.get(watch.video.source.catalog);
		const renditions = videoCatalog?.renditions ?? {};

		const renditionsList: Rendition[] = Object.entries(renditions).map(([name, config]) => ({
			name,
			width: config.codedWidth,
			height: config.codedHeight,
		}));

		setAvailableRenditions(renditionsList);
	});

	signals.run((effect) => {
		const selected = effect.get(watch.video.source.track);
		setActiveRendition(selected);
	});

	const handleFullscreenChange = () => {
		setIsFullscreen(!!document.fullscreenElement);
	};

	signals.event(document, "fullscreenchange", handleFullscreenChange);
	onCleanup(() => signals.close());

	return <WatchUIContext.Provider value={value}>{props.children}</WatchUIContext.Provider>;
}
