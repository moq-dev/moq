// One CARLA camera feeding the demo's GPU video bridge (demo/carla/bridge).
//
// The bridge library is loaded at runtime and listens on the socket named by
// `-moq-bridge-socket=`. Each frame the camera's render target is copied on
// Unreal's own Vulkan queue into an exportable image that the bridge's encoder
// imports on the same device, converts, scales, and encodes. Pixels never
// leave the GPU, and there is no CPU path: a failure stops the camera, and the
// bridge reports it to the worker.

#pragma once

#include "CoreMinimal.h"

class UTextureRenderTarget2D;

class MOQBRIDGE_API IMoqBridgeCamera
{
public:
  /// Register `Target`'s camera as a source of `Stream` (the worker's
  /// broadcast, e.g. "truck1/front") and allocate its slots on Unreal's
  /// device. Game thread, once the render target is initialized. Null with
  /// `OutError` set on failure.
  static TSharedPtr<IMoqBridgeCamera, ESPMode::ThreadSafe> Open(
      const FString &Stream,
      const UTextureRenderTarget2D &Target,
      FString &OutError);

  /// Closes the source, which waits until the GPU is done with every slot,
  /// then frees them. Game thread, with no capture pending: flush rendering
  /// commands before dropping the last reference.
  virtual ~IMoqBridgeCamera() = default;

  /// Whether the bridge wants this camera's pictures now: render only if so.
  /// False once the camera has failed.
  virtual bool IsWanted() const = 0;

  /// Why the camera stopped, or empty while it works.
  virtual FString GetError() const = 0;

  /// Copy `Target` into a free slot and hand it to the encoder, after the
  /// rendering enqueued before this call. Game thread. `TimestampUs` is the
  /// simulation time of the picture.
  virtual void Capture(UTextureRenderTarget2D &Target, uint64 Frame, uint64 TimestampUs) = 0;
};
