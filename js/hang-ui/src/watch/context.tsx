import { type Moq, Signals } from "@moq/hang";
import type * as Catalog from "@moq/hang/catalog";
import type HangWatch from "@moq/hang/watch/element";
import type { JSX } from "solid-js";
import { createContext, createSignal, onCleanup } from "solid-js";
import type { Time } from "@moq/lite";

type WatchUIContextProviderProps = {
	hangWatch: HangWatch;
	children: JSX.Element;
};

type WatchStatus = "no-url" | "disconnected" | "connecting" | "offline" | "loading" | "live" | "connected";

export type Rendition = {
	name: string;
	width?: number;
	height?: number;
};

export type WatchUIContextValues = {
	hangWatch: HangWatch;
	watchStatus: () => WatchStatus;
	isPlaying: () => boolean;
	isMuted: () => boolean;
	setVolume: (vol: number) => void;
	currentVolume: () => number;
	togglePlayback: () => void;
	toggleMuted: () => void;
	buffering: () => boolean;
	latency: () => number;
	setLatencyValue: (value: number) => void;
	availableRenditions: () => Rendition[];
	activeRendition: () => string | undefined;
	setActiveRendition: (name: string | undefined) => void;
	isStatsPanelVisible: () => boolean;
	setIsStatsPanelVisible: (visible: boolean) => void;
	isBufferOverlayVisible: () => boolean;
	setIsBufferOverlayVisible: (visible: boolean) => void;
	videoBufferedRanges: () => { start: Time.Micro | undefined, end: Time.Micro | undefined }[];
	audioBufferedRanges: () => { start: Time.Micro | undefined, end: Time.Micro | undefined }[];
	videoCurrentTime: () => Time.Micro | undefined;
	isFullscreen: () => boolean;
	toggleFullscreen: () => void;
};

export const WatchUIContext = createContext<WatchUIContextValues>();

export default function WatchUIContextProvider(props: WatchUIContextProviderProps) {
	const [watchStatus, setWatchStatus] = createSignal<WatchStatus>("no-url");
	const [isPlaying, setIsPlaying] = createSignal<boolean>(false);
	const [isMuted, setIsMuted] = createSignal<boolean>(false);
	const [currentVolume, setCurrentVolume] = createSignal<number>(0);
	const [buffering, setBuffering] = createSignal<boolean>(false);
	const [latency, setLatency] = createSignal<number>(0);
	const [availableRenditions, setAvailableRenditions] = createSignal<Rendition[]>([]);
	const [activeRendition, setActiveRendition] = createSignal<string | undefined>(undefined);
	const [isStatsPanelVisible, setIsStatsPanelVisibleInternal] = createSignal<boolean>(false);
	const [isBufferOverlayVisible, setIsBufferOverlayVisibleInternal] = createSignal<boolean>(false);
	const [videoBufferedRanges, setVideoBufferedRanges] = createSignal<{ start: Time.Micro | undefined, end: Time.Micro | undefined }[]>([]);
	const [audioBufferedRanges, setAudioBufferedRanges] = createSignal<{ start: Time.Micro | undefined, end: Time.Micro | undefined }[]>([]);
	const [videoCurrentTime, setVideoCurrentTime] = createSignal<Time.Micro | undefined>(undefined);

	// Mutual exclusivity: hide buffer overlay when stats panel is shown and vice versa
	const setIsStatsPanelVisible = (visible: boolean) => {
		if (visible) setIsBufferOverlayVisibleInternal(false);
		setIsStatsPanelVisibleInternal(visible);
	};

	const setIsBufferOverlayVisible = (visible: boolean) => {
		if (visible) setIsStatsPanelVisibleInternal(false);
		setIsBufferOverlayVisibleInternal(visible);
	};
	const [isFullscreen, setIsFullscreen] = createSignal<boolean>(!!document.fullscreenElement);

	const togglePlayback = () => {
		props.hangWatch.paused.set(!props.hangWatch.paused.get());
	};

	const toggleFullscreen = () => {
		if (document.fullscreenElement) {
			document.exitFullscreen();
		} else {
			props.hangWatch.requestFullscreen();
		}
	};

	const setVolume = (volume: number) => {
		props.hangWatch.volume.set(volume / 100);
	};

	const toggleMuted = () => {
		props.hangWatch.muted.update((muted) => !muted);
	};

	const setLatencyValue = (latency: number) => {
		props.hangWatch.latency.set(latency as Moq.Time.Milli);
	};

	const setActiveRenditionValue = (name: string | undefined) => {
		props.hangWatch.video.source.target.update((prev) => ({
			...prev,
			rendition: name,
		}));
	};

	const value: WatchUIContextValues = {
		hangWatch: props.hangWatch,
		watchStatus,
		togglePlayback,
		isPlaying,
		setVolume,
		isMuted,
		currentVolume,
		toggleMuted,
		buffering,
		latency,
		setLatencyValue,
		availableRenditions,
		activeRendition,
		setActiveRendition: setActiveRenditionValue,
		isStatsPanelVisible,
		setIsStatsPanelVisible,
		isBufferOverlayVisible,
		setIsBufferOverlayVisible,
		videoBufferedRanges,
		audioBufferedRanges,
		videoCurrentTime,
		isFullscreen,
		toggleFullscreen,
	};

	const watch = props.hangWatch;
	const signals = new Signals.Effect();

	signals.effect((effect) => {
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

	signals.effect((effect) => {
		const paused = effect.get(watch.video.paused);
		setIsPlaying(!paused);
	});

	signals.effect((effect) => {
		const volume = effect.get(watch.audio.volume);
		setCurrentVolume(volume * 100);
	});

	signals.effect((effect) => {
		const muted = effect.get(watch.audio.muted);
		setIsMuted(muted);
	});

	signals.effect((effect) => {
		const syncStatus = effect.get(watch.video.source.syncStatus);
		const bufferStatus = effect.get(watch.video.source.bufferStatus);
		const shouldShow = syncStatus.state === "wait" || bufferStatus.state === "empty";

		setBuffering(shouldShow);
	});

	signals.effect((effect) => {
		const latency = effect.get(watch.latency);
		setLatency(latency);
	});

	signals.effect((effect) => {
		const rootCatalog = effect.get(watch.broadcast.catalog);
		const videoCatalog = rootCatalog?.video;
		const renditions = videoCatalog?.renditions ?? {};

		const renditionsList: Rendition[] = Object.entries(renditions).map(([name, config]) => ({
			name,
			width: (config as Catalog.VideoConfig).codedWidth,
			height: (config as Catalog.VideoConfig).codedHeight,
		}));

		setAvailableRenditions(renditionsList);
	});

	signals.effect((effect) => {
		const selected = effect.get(watch.video.source.active);
		setActiveRendition(selected);
	});

	signals.effect((effect) => {
		const bufferedRanges = effect.get(watch.video.source.bufferedRanges);
		setVideoBufferedRanges(bufferedRanges);
	});
	
	signals.effect((effect) => {
		const bufferedRanges = effect.get(watch.audio.source.bufferedRanges);
		setAudioBufferedRanges(bufferedRanges);
	});

	signals.effect((effect) => {
		const stats = effect.get(watch.video.source.stats);
		// Use stats.timestamp directly - buffers are positioned relative to decoded frame time
		setVideoCurrentTime(stats?.timestamp as Time.Micro | undefined);
	});

	const handleFullscreenChange = () => {
		setIsFullscreen(!!document.fullscreenElement);
	};

	signals.event(document, "fullscreenchange", handleFullscreenChange);
	onCleanup(() => signals.close());

	return <WatchUIContext.Provider value={value}>{props.children}</WatchUIContext.Provider>;
}
