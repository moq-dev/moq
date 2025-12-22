import * as Moq from "@moq/lite";
import * as Catalog from "@moq/hang/catalog";

export interface MoqClientConfig {
  relayUrl: string;
  broadcastName: string;
  onData?: (data: Uint8Array, trackType: "video" | "audio") => void;
  onCatalog?: (catalog: Catalog.Root) => void;
  onError?: (error: Error) => void;
  onConnected?: () => void;
  onDisconnected?: () => void;
}

export class MoqClient {
  private connection: Moq.Connection.Established | null = null;
  private broadcast: Moq.Broadcast | null = null;
  private config: MoqClientConfig;
  private isRunning = false;

  constructor(config: MoqClientConfig) {
    this.config = config;
  }

  async connect(): Promise<void> {
    try {
      this.isRunning = true;

      // Connect to relay using @moq/lite
      this.connection = await Moq.Connection.connect(new URL(this.config.relayUrl));
      console.log("[MoqClient] Connected to relay:", this.config.relayUrl);

      this.config.onConnected?.();

      // Subscribe to broadcast
      this.broadcast = this.connection.consume(Moq.Path.from(this.config.broadcastName));
      console.log("[MoqClient] Subscribed to broadcast:", this.config.broadcastName);

      // First, fetch the catalog to get track names
      const catalog = await this.fetchCatalog();

      if (!catalog) {
        console.warn("[MoqClient] No catalog received, using default track names");
        // Fallback to default names
        await this.subscribeToTracks("video0", "audio1");
      } else {
        this.config.onCatalog?.(catalog);

        // Get track names from catalog
        const videoTrackName = this.getVideoTrackName(catalog);
        const audioTrackName = this.getAudioTrackName(catalog);

        console.log("[MoqClient] Using tracks from catalog:", { videoTrackName, audioTrackName });

        await this.subscribeToTracks(videoTrackName, audioTrackName);
      }
    } catch (error) {
      console.error("[MoqClient] Connection error:", error);
      this.config.onError?.(error as Error);
      throw error;
    }
  }

  private async fetchCatalog(): Promise<Catalog.Root | null> {
    if (!this.broadcast) return null;

    console.log("[MoqClient] Fetching catalog.json...");
    const catalogTrack = this.broadcast.subscribe("catalog.json", 100);

    try {
      // Wait for catalog with timeout
      const frame = await Promise.race([
        catalogTrack.readFrame(),
        new Promise<null>((resolve) => setTimeout(() => resolve(null), 5000))
      ]);

      if (!frame) {
        console.warn("[MoqClient] Catalog fetch timed out");
        return null;
      }

      // Use the catalog decode function from @moq/hang
      const catalog = Catalog.decode(frame);
      console.log("[MoqClient] Received catalog:", catalog);

      return catalog;
    } catch (error) {
      console.warn("[MoqClient] Error fetching catalog:", error);
      return null;
    }
  }

  private getVideoTrackName(catalog: Catalog.Root): string | null {
    if (!catalog.video?.renditions) return null;

    // The rendition key (e.g., "video0") IS the track name
    const renditionKeys = Object.keys(catalog.video.renditions);
    if (renditionKeys.length > 0) {
      return renditionKeys[0];
    }
    return null;
  }

  private getAudioTrackName(catalog: Catalog.Root): string | null {
    if (!catalog.audio?.renditions) return null;

    // The rendition key (e.g., "audio1") IS the track name
    const renditionKeys = Object.keys(catalog.audio.renditions);
    if (renditionKeys.length > 0) {
      return renditionKeys[0];
    }
    return null;
  }

  private async subscribeToTracks(
    videoTrackName: string | null,
    audioTrackName: string | null
  ): Promise<void> {
    if (!this.broadcast) return;

    const promises: Promise<void>[] = [];

    if (videoTrackName) {
      const videoTrack = this.broadcast.subscribe(videoTrackName, 2);
      console.log("[MoqClient] Subscribed to video track:", videoTrackName);
      promises.push(this.processTrack(videoTrack, "video"));
    }

    if (audioTrackName) {
      const audioTrack = this.broadcast.subscribe(audioTrackName, 2);
      console.log("[MoqClient] Subscribed to audio track:", audioTrackName);
      promises.push(this.processTrack(audioTrack, "audio"));
    }

    await Promise.all(promises);
  }

  private async processTrack(
    track: Moq.Track,
    trackType: "video" | "audio"
  ): Promise<void> {
    console.log(`[MoqClient] Processing ${trackType} track:`, track.name);

    try {
      while (this.isRunning) {
        const group = await track.nextGroup();
        if (!group) {
          console.log(`[MoqClient] ${trackType} track ended`);
          break;
        }

        while (this.isRunning) {
          const frame = await group.readFrame();
          if (!frame) break;

          // For segment mode, the frame IS the complete fMP4 segment
          // No timestamp stripping needed
          this.config.onData?.(frame, trackType);
        }
      }
    } catch (error) {
      if (this.isRunning) {
        console.error(`[MoqClient] Error processing ${trackType} track:`, error);
        this.config.onError?.(error as Error);
      }
    }
  }

  disconnect(): void {
    this.isRunning = false;
    if (this.broadcast) {
      this.broadcast.close();
      this.broadcast = null;
    }
    if (this.connection) {
      this.connection.close();
      this.connection = null;
    }
    this.config.onDisconnected?.();
    console.log("[MoqClient] Disconnected");
  }
}
