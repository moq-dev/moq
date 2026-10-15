// See MoqBridgeCamera.h. The protocol with the bridge library is the one
// carla_bridge.h documents, and demo/carla/bridge/tests/standin is its reference
// producer: this follows it step for step on Unreal's device and queue.

#include "MoqBridgeCamera.h"

#include "Engine/TextureRenderTarget2D.h"
#include "HAL/PlatformProcess.h"
#include "Misc/Paths.h"
#include "RenderUtils.h"
#include "RenderingThread.h"
#include "TextureResource.h"

#include "VulkanRHIPrivate.h"
#include "VulkanContext.h"
#include "VulkanPendingState.h"

#include "MoqBridgeSettings.h"
#include "carla_bridge.h"

#include <atomic>
#include <dlfcn.h>
#include <unistd.h>

DEFINE_LOG_CATEGORY_STATIC(LogMoqBridge, Log, All);

namespace
{
  /// Enough to render one picture while the bridge still converts the last
  /// two; with none idle, a picture is dropped before anything encodes it.
  constexpr uint32 SLOTS = 3;

  /// The library's entry points. It is loaded, not linked, so a stock launch
  /// never needs it and a missing one is an error naming the path.
  struct FApi
  {
    decltype(&carla_bridge_source_open) Open = nullptr;
    decltype(&carla_bridge_source_import) Import = nullptr;
    decltype(&carla_bridge_source_wanted) Wanted = nullptr;
    decltype(&carla_bridge_source_acquire) Acquire = nullptr;
    decltype(&carla_bridge_source_submit) Submit = nullptr;
    decltype(&carla_bridge_source_cancel) Cancel = nullptr;
    decltype(&carla_bridge_source_fail) Fail = nullptr;
    decltype(&carla_bridge_source_close) Close = nullptr;
    decltype(&carla_bridge_error) Error = nullptr;

    FString LastError() const
    {
      return UTF8_TO_TCHAR(Error());
    }
  };

  template <typename T>
  bool Resolve(void *Library, const char *Name, T &Out, FString &OutError)
  {
    Out = reinterpret_cast<T>(dlsym(Library, Name));
    if (Out == nullptr)
    {
      OutError = FString::Printf(TEXT("the bridge library has no %s"), UTF8_TO_TCHAR(Name));
      return false;
    }
    return true;
  }

  /// Loaded once per process, from `-moq-bridge-library=` or beside the
  /// executable. Never unloaded: the bridge owns threads for the process's
  /// life.
  const FApi *LoadApi(FString &OutError)
  {
    static FCriticalSection Lock;
    static bool bLoaded = false;
    static FApi Api;
    static FString Error;

    FScopeLock Scope(&Lock);
    if (!bLoaded)
    {
      bLoaded = true;
      FString Path;
      if (!FParse::Value(FCommandLine::Get(), TEXT("moq-bridge-library="), Path))
      {
        Path = FPaths::Combine(FPlatformProcess::BaseDir(), TEXT("libcarla_bridge.so"));
      }
      void *Library = dlopen(TCHAR_TO_UTF8(*Path), RTLD_NOW | RTLD_LOCAL);
      if (Library == nullptr)
      {
        Error = FString::Printf(TEXT("load %s: %s"), *Path, UTF8_TO_TCHAR(dlerror()));
      }
      else if (
          !Resolve(Library, "carla_bridge_source_open", Api.Open, Error) ||
          !Resolve(Library, "carla_bridge_source_import", Api.Import, Error) ||
          !Resolve(Library, "carla_bridge_source_wanted", Api.Wanted, Error) ||
          !Resolve(Library, "carla_bridge_source_acquire", Api.Acquire, Error) ||
          !Resolve(Library, "carla_bridge_source_submit", Api.Submit, Error) ||
          !Resolve(Library, "carla_bridge_source_cancel", Api.Cancel, Error) ||
          !Resolve(Library, "carla_bridge_source_fail", Api.Fail, Error) ||
          !Resolve(Library, "carla_bridge_source_close", Api.Close, Error) ||
          !Resolve(Library, "carla_bridge_error", Api.Error, Error))
      {
        Error = FString::Printf(TEXT("%s: %s"), *Path, *Error);
      }
    }
    if (!Error.IsEmpty())
    {
      OutError = Error;
      return nullptr;
    }
    return &Api;
  }

  /// Device entry points of the extensions the module enables. The loader
  /// returns null for an extension the device was created without, which is
  /// how a device missing one is refused here rather than misused.
  struct FDeviceApi
  {
    PFN_vkGetMemoryFdKHR GetMemoryFd = nullptr;
    PFN_vkGetSemaphoreFdKHR GetSemaphoreFd = nullptr;
    PFN_vkWaitSemaphoresKHR WaitSemaphores = nullptr;

    bool Load(VkDevice Device, FString &OutError)
    {
      GetMemoryFd = reinterpret_cast<PFN_vkGetMemoryFdKHR>(
          VulkanRHI::vkGetDeviceProcAddr(Device, "vkGetMemoryFdKHR"));
      GetSemaphoreFd = reinterpret_cast<PFN_vkGetSemaphoreFdKHR>(
          VulkanRHI::vkGetDeviceProcAddr(Device, "vkGetSemaphoreFdKHR"));
      WaitSemaphores = reinterpret_cast<PFN_vkWaitSemaphoresKHR>(
          VulkanRHI::vkGetDeviceProcAddr(Device, "vkWaitSemaphoresKHR"));
      if (GetMemoryFd == nullptr || GetSemaphoreFd == nullptr || WaitSemaphores == nullptr)
      {
        OutError = TEXT("the Vulkan device lacks external memory, external semaphores or timeline "
                        "semaphores; is the MoqBridge module loaded and the engine patched?");
        return false;
      }
      return true;
    }
  };

  struct FSlot
  {
    VkImage Image = VK_NULL_HANDLE;
    VkDeviceMemory Memory = VK_NULL_HANDLE;
    VkSemaphore Timeline = VK_NULL_HANDLE;
    /// The last value a submission signals, which teardown waits for.
    uint64_t Signaled = 0;
    /// Whether the image holds a picture, so is in GENERAL rather than
    /// UNDEFINED. Only the RHI thread touches it.
    bool bWritten = false;
  };

  class FMoqBridgeCamera final
      : public IMoqBridgeCamera,
        public TSharedFromThis<FMoqBridgeCamera, ESPMode::ThreadSafe>
  {
  public:
    explicit FMoqBridgeCamera(const FApi &InApi) : Api(InApi) {}

    bool Init(const FString &Socket, const FString &Stream, uint32 Width, uint32 Height, FString &OutError)
    {
      Device = GVulkanRHI->GetDevice();
      const VkDevice Handle = Device->GetInstanceHandle();
      if (!DeviceApi.Load(Handle, OutError))
      {
        return false;
      }

      Source = Api.Open(
          TCHAR_TO_UTF8(*Socket),
          TCHAR_TO_UTF8(*Stream),
          Device->GetDeviceIdProperties().deviceUUID,
          Device->GetDeviceIdProperties().driverUUID,
          Width,
          Height,
          CARLA_BRIDGE_BGRA,
          SLOTS);
      if (Source == nullptr)
      {
        OutError = FString::Printf(TEXT("open %s on %s: %s"), *Stream, *Socket, *Api.LastError());
        return false;
      }

      VkPhysicalDeviceMemoryProperties Memory;
      VulkanRHI::vkGetPhysicalDeviceMemoryProperties(Device->GetPhysicalHandle(), &Memory);
      Slots.SetNum(SLOTS);
      for (uint32 Index = 0; Index < SLOTS; ++Index)
      {
        if (!CreateSlot(Index, Width, Height, Memory, OutError))
        {
          OutError = FString::Printf(TEXT("%s slot %u: %s"), *Stream, Index, *OutError);
          return false;
        }
      }
      return true;
    }

    /// Close before freeing: the bridge returns once its encoders are done
    /// with every slot. A copy submitted but never handed over (a failed submit) is
    /// waited for through its own timeline, or through the device when even
    /// its signal failed.
    ~FMoqBridgeCamera() override
    {
      check(IsInGameThread());
      if (Source != nullptr)
      {
        Api.Close(Source);
      }
      if (Device == nullptr)
      {
        return;
      }
      const VkDevice Handle = Device->GetInstanceHandle();
      if (bUnsignaledCopy)
      {
        // Rendering commands are flushed, so nothing else submits meanwhile.
        Device->WaitUntilIdle();
      }
      for (FSlot &Slot : Slots)
      {
        if (Slot.Signaled > 0)
        {
          VkSemaphoreWaitInfoKHR Wait;
          ZeroVulkanStruct(Wait, VK_STRUCTURE_TYPE_SEMAPHORE_WAIT_INFO_KHR);
          Wait.semaphoreCount = 1;
          Wait.pSemaphores = &Slot.Timeline;
          Wait.pValues = &Slot.Signaled;
          const VkResult Result = DeviceApi.WaitSemaphores(Handle, &Wait, UINT64_MAX);
          if (Result != VK_SUCCESS)
          {
            // The device is gone; freeing is all that is left to do.
            UE_LOG(LogMoqBridge, Error, TEXT("waiting for a slot's last copy: VkResult %d"), int32(Result));
          }
        }
        if (Slot.Timeline != VK_NULL_HANDLE)
        {
          VulkanRHI::vkDestroySemaphore(Handle, Slot.Timeline, nullptr);
        }
        if (Slot.Image != VK_NULL_HANDLE)
        {
          VulkanRHI::vkDestroyImage(Handle, Slot.Image, nullptr);
        }
        if (Slot.Memory != VK_NULL_HANDLE)
        {
          VulkanRHI::vkFreeMemory(Handle, Slot.Memory, nullptr);
        }
      }
    }

    bool IsWanted() const override
    {
      return !bFailed.load(std::memory_order_acquire) && Api.Wanted(Source);
    }

    FString GetError() const override
    {
      FScopeLock Scope(&ErrorLock);
      return Error;
    }

    void Capture(UTextureRenderTarget2D &Target, uint64 Frame, uint64 TimestampUs) override
    {
      check(IsInGameThread());
      FTextureRenderTargetResource *Resource = Target.GameThread_GetRenderTargetResource();
      if (Resource == nullptr)
      {
        Fail(TEXT("the render target has no resource"));
        return;
      }
      // The camera rides along, so it outlives every copy still queued.
      ENQUEUE_RENDER_COMMAND(MoqBridgeCapture)(
          [Camera = AsShared(), Resource, Frame, TimestampUs](FRHICommandListImmediate &RHICmdList)
          {
            FTexture2DRHIRef Texture = Resource->GetRenderTargetTexture();
            // After everything the render thread has recorded so far,
            // including the capture itself, on the thread that submits.
            RHICmdList.EnqueueLambda(
                [Camera, Texture, Frame, TimestampUs](FRHICommandListImmediate &)
                {
                  Camera->Copy(Texture, Frame, TimestampUs);
                });
          });
    }

  private:
    bool CreateSlot(
        uint32 Index,
        uint32 Width,
        uint32 Height,
        const VkPhysicalDeviceMemoryProperties &Memory,
        FString &OutError)
    {
      const VkDevice Handle = Device->GetInstanceHandle();
      FSlot &Slot = Slots[Index];

      VkExternalMemoryImageCreateInfo External;
      ZeroVulkanStruct(External, VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO);
      External.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
      VkImageCreateInfo Image;
      ZeroVulkanStruct(Image, VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO);
      Image.pNext = &External;
      Image.imageType = VK_IMAGE_TYPE_2D;
      Image.format = VK_FORMAT_B8G8R8A8_UNORM;
      Image.extent = {Width, Height, 1};
      Image.mipLevels = 1;
      Image.arrayLayers = 1;
      Image.samples = VK_SAMPLE_COUNT_1_BIT;
      Image.tiling = VK_IMAGE_TILING_OPTIMAL;
      Image.usage = VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_STORAGE_BIT;
      Image.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
      Image.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
      if (!Check(VulkanRHI::vkCreateImage(Handle, &Image, nullptr, &Slot.Image), TEXT("create the image"), OutError))
      {
        return false;
      }

      VkMemoryRequirements Requirements;
      VulkanRHI::vkGetImageMemoryRequirements(Handle, Slot.Image, &Requirements);
      int32 Type = -1;
      for (uint32 Candidate = 0; Candidate < Memory.memoryTypeCount; ++Candidate)
      {
        if ((Requirements.memoryTypeBits & (1u << Candidate)) != 0 &&
            (Memory.memoryTypes[Candidate].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) != 0)
        {
          Type = int32(Candidate);
          break;
        }
      }
      if (Type < 0)
      {
        OutError = TEXT("no device-local memory type for the image");
        return false;
      }
      VkMemoryDedicatedAllocateInfo Dedicated;
      ZeroVulkanStruct(Dedicated, VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO);
      Dedicated.image = Slot.Image;
      VkExportMemoryAllocateInfo Export;
      ZeroVulkanStruct(Export, VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO);
      Export.pNext = &Dedicated;
      Export.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
      VkMemoryAllocateInfo Allocation;
      ZeroVulkanStruct(Allocation, VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO);
      Allocation.pNext = &Export;
      Allocation.allocationSize = Requirements.size;
      Allocation.memoryTypeIndex = uint32(Type);
      if (!Check(VulkanRHI::vkAllocateMemory(Handle, &Allocation, nullptr, &Slot.Memory), TEXT("allocate the image"), OutError) ||
          !Check(VulkanRHI::vkBindImageMemory(Handle, Slot.Image, Slot.Memory, 0), TEXT("bind the image"), OutError))
      {
        return false;
      }

      VkSemaphoreTypeCreateInfoKHR Kind;
      ZeroVulkanStruct(Kind, VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO_KHR);
      Kind.semaphoreType = VK_SEMAPHORE_TYPE_TIMELINE_KHR;
      Kind.initialValue = 0;
      VkExportSemaphoreCreateInfo ExportSemaphore;
      ZeroVulkanStruct(ExportSemaphore, VK_STRUCTURE_TYPE_EXPORT_SEMAPHORE_CREATE_INFO);
      ExportSemaphore.pNext = &Kind;
      ExportSemaphore.handleTypes = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT;
      VkSemaphoreCreateInfo Semaphore;
      ZeroVulkanStruct(Semaphore, VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO);
      Semaphore.pNext = &ExportSemaphore;
      if (!Check(VulkanRHI::vkCreateSemaphore(Handle, &Semaphore, nullptr, &Slot.Timeline), TEXT("create the timeline"), OutError))
      {
        return false;
      }

      int MemoryFd = -1;
      int TimelineFd = -1;
      VkMemoryGetFdInfoKHR MemoryInfo;
      ZeroVulkanStruct(MemoryInfo, VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR);
      MemoryInfo.memory = Slot.Memory;
      MemoryInfo.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
      if (!Check(DeviceApi.GetMemoryFd(Handle, &MemoryInfo, &MemoryFd), TEXT("export the image"), OutError))
      {
        return false;
      }
      VkSemaphoreGetFdInfoKHR TimelineInfo;
      ZeroVulkanStruct(TimelineInfo, VK_STRUCTURE_TYPE_SEMAPHORE_GET_FD_INFO_KHR);
      TimelineInfo.semaphore = Slot.Timeline;
      TimelineInfo.handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT;
      if (!Check(DeviceApi.GetSemaphoreFd(Handle, &TimelineInfo, &TimelineFd), TEXT("export the timeline"), OutError))
      {
        close(MemoryFd);
        return false;
      }
      // Consumes both descriptors whatever it returns.
      if (Api.Import(Source, Index, MemoryFd, TimelineFd, Requirements.size, uint32(Type)) != 0)
      {
        OutError = FString::Printf(TEXT("import: %s"), *Api.LastError());
        return false;
      }
      return true;
    }

    static bool Check(VkResult Result, const TCHAR *What, FString &OutError)
    {
      if (Result == VK_SUCCESS)
      {
        return true;
      }
      OutError = FString::Printf(TEXT("%s: VkResult %d"), What, int32(Result));
      return false;
    }

    /// The RHI thread's half: carla_bridge.h's per-frame sequence, recorded
    /// into Unreal's active command buffer after the capture.
    void Copy(FRHITexture *Texture, uint64 Frame, uint64 TimestampUs)
    {
      if (bFailed.load(std::memory_order_acquire))
      {
        return;
      }
      uint32 Index = 0;
      uint64_t Ready = 0;
      const int Acquired = Api.Acquire(Source, &Index, &Ready);
      if (Acquired < 0)
      {
        Fail(FString::Printf(TEXT("acquire: %s"), *Api.LastError()));
        return;
      }
      if (Acquired == 0)
      {
        // Every slot is still being encoded: drop this picture.
        return;
      }
      FSlot &Slot = Slots[Index];

      FVulkanCommandListContextImmediate &Context = Device->GetImmediateContext();
      FVulkanCommandBufferManager *Manager = Context.GetCommandBufferManager();
      FVulkanCmdBuffer *CmdBuffer = Manager->GetActiveCmdBuffer();
      if (!CmdBuffer->IsOutsideRenderPass())
      {
        Abandon(Index, TEXT("the capture ended inside a render pass"));
        return;
      }

      // The render target stays in whatever layout Unreal tracks for it: the
      // copy moves it to TRANSFER_SRC and back, so the tracking stays true.
      const VkImage Target = FVulkanTextureBase::Cast(Texture)->Surface.Image;
      FVulkanImageLayout *Tracked = Context.GetLayoutManager().GetFullLayout(Target);
      if (Tracked == nullptr || !Tracked->AreAllSubresourcesSameLayout() ||
          Tracked->MainLayout == VK_IMAGE_LAYOUT_UNDEFINED)
      {
        Abandon(Index, TEXT("the render target has no single tracked layout"));
        return;
      }
      const VkImageLayout Layout = Tracked->MainLayout;

      VkImageSubresourceRange Range = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
      auto Barrier = [&Range](VkImage Image, VkImageLayout From, VkImageLayout To, VkAccessFlags Src, VkAccessFlags Dst)
      {
        VkImageMemoryBarrier Out;
        ZeroVulkanStruct(Out, VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER);
        Out.oldLayout = From;
        Out.newLayout = To;
        Out.srcAccessMask = Src;
        Out.dstAccessMask = Dst;
        Out.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
        Out.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
        Out.image = Image;
        Out.subresourceRange = Range;
        return Out;
      };
      const VkCommandBuffer Commands = CmdBuffer->GetHandle();
      // After every earlier use of both images on this queue: the capture
      // that wrote the target, and the last copy into the slot.
      const VkImageMemoryBarrier Before[] = {
          Barrier(Target, Layout, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                  VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_TRANSFER_READ_BIT),
          Barrier(Slot.Image, Slot.bWritten ? VK_IMAGE_LAYOUT_GENERAL : VK_IMAGE_LAYOUT_UNDEFINED,
                  VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                  VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT, VK_ACCESS_TRANSFER_WRITE_BIT),
      };
      VulkanRHI::vkCmdPipelineBarrier(
          Commands, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0,
          0, nullptr, 0, nullptr, UE_ARRAY_COUNT(Before), Before);
      VkImageCopy Region;
      FMemory::Memzero(Region);
      Region.srcSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
      Region.dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
      const FIntVector Size = Texture->GetSizeXYZ();
      Region.extent = {uint32(Size.X), uint32(Size.Y), 1};
      VulkanRHI::vkCmdCopyImage(
          Commands, Target, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
          Slot.Image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &Region);
      // The slot is left in GENERAL for the encoder; the target goes back to its
      // tracked layout for whatever Unreal does with it next.
      const VkImageMemoryBarrier After[] = {
          Barrier(Target, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, Layout,
                  0, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT),
          Barrier(Slot.Image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_GENERAL,
                  VK_ACCESS_TRANSFER_WRITE_BIT, VK_ACCESS_MEMORY_READ_BIT),
      };
      VulkanRHI::vkCmdPipelineBarrier(
          Commands, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0,
          0, nullptr, 0, nullptr, UE_ARRAY_COUNT(After), After);
      Slot.bWritten = true;

      // Unreal's submission cannot signal a timeline value, so an empty one
      // follows it on the same queue: a signal covers every command submitted
      // before it.
      Manager->SubmitActiveCmdBuffer();
      Manager->PrepareForNewActiveCommandBuffer();
      VkTimelineSemaphoreSubmitInfoKHR Timeline;
      ZeroVulkanStruct(Timeline, VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO_KHR);
      Timeline.signalSemaphoreValueCount = 1;
      Timeline.pSignalSemaphoreValues = &Ready;
      VkSubmitInfo Submit;
      ZeroVulkanStruct(Submit, VK_STRUCTURE_TYPE_SUBMIT_INFO);
      Submit.pNext = &Timeline;
      Submit.signalSemaphoreCount = 1;
      Submit.pSignalSemaphores = &Slot.Timeline;
      const VkResult Result = VulkanRHI::vkQueueSubmit(
          Device->GetGraphicsQueue()->GetHandle(), 1, &Submit, VK_NULL_HANDLE);
      if (Result != VK_SUCCESS)
      {
        // The copy is already on the queue with no timeline value to wait
        // for, so teardown waits for the whole device instead.
        bUnsignaledCopy = true;
        Abandon(Index, FString::Printf(TEXT("signal the slot: VkResult %d"), int32(Result)));
        return;
      }
      Slot.Signaled = Ready;

      if (Api.Submit(Source, Index, Frame, TimestampUs) != 0)
      {
        Fail(FString::Printf(TEXT("submit: %s"), *Api.LastError()));
      }
    }

    /// Hand an acquired slot back unused, then stop.
    void Abandon(uint32 Index, const FString &Why)
    {
      if (Api.Cancel(Source, Index) != 0)
      {
        Fail(FString::Printf(TEXT("%s; cancel: %s"), *Why, *Api.LastError()));
        return;
      }
      Fail(Why);
    }

    void Fail(const FString &Why)
    {
      {
        FScopeLock Scope(&ErrorLock);
        if (!Error.IsEmpty())
        {
          return;
        }
        Error = Why;
      }
      bFailed.store(true, std::memory_order_release);
      UE_LOG(LogMoqBridge, Error, TEXT("camera stopped: %s"), *Why);
      // The worker would otherwise go on waiting for this camera's video.
      if (Api.Fail(Source, TCHAR_TO_UTF8(*Why)) != 0)
      {
        UE_LOG(LogMoqBridge, Error, TEXT("reporting it: %s"), *Api.LastError());
      }
    }

    const FApi &Api;
    FDeviceApi DeviceApi;
    FVulkanDevice *Device = nullptr;
    CarlaBridgeSource *Source = nullptr;
    TArray<FSlot> Slots;
    std::atomic<bool> bFailed{false};
    /// A copy reached the queue but its timeline signal did not. Set on the
    /// RHI thread, read by the destructor after rendering commands flush.
    bool bUnsignaledCopy = false;
    mutable FCriticalSection ErrorLock;
    FString Error;
  };
}

TSharedPtr<IMoqBridgeCamera, ESPMode::ThreadSafe> IMoqBridgeCamera::Open(
    const FString &Stream,
    const UTextureRenderTarget2D &Target,
    FString &OutError)
{
  check(IsInGameThread());
  FString Socket;
  if (!MoqBridge::GetSocket(Socket))
  {
    OutError = TEXT("the simulator was started without -moq-bridge-socket=");
    return nullptr;
  }
  if (GDynamicRHI == nullptr || FCString::Strcmp(GDynamicRHI->GetName(), TEXT("Vulkan")) != 0)
  {
    OutError = TEXT("the bridge needs the Vulkan RHI");
    return nullptr;
  }
  if (Target.GetFormat() != PF_B8G8R8A8)
  {
    OutError = FString::Printf(TEXT("the render target is %s, not B8G8R8A8"), GetPixelFormatString(Target.GetFormat()));
    return nullptr;
  }
  const FApi *Api = LoadApi(OutError);
  if (Api == nullptr)
  {
    return nullptr;
  }
  // A camera that fails partway is freed by its destructor, which closes
  // whatever it registered.
  TSharedRef<FMoqBridgeCamera, ESPMode::ThreadSafe> Camera = MakeShared<FMoqBridgeCamera, ESPMode::ThreadSafe>(*Api);
  if (!Camera->Init(Socket, Stream, Target.SizeX, Target.SizeY, OutError))
  {
    return nullptr;
  }
  UE_LOG(LogMoqBridge, Log, TEXT("%s: %dx%d camera on %s"), *Stream, Target.SizeX, Target.SizeY, *Socket);
  return Camera;
}
