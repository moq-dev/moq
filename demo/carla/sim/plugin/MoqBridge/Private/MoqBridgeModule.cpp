// Loaded at PostConfigInit, before the RHI exists, so the Vulkan device is
// created with what the bridge shares slots through: exportable memory and
// timeline semaphores. The engine patch next to this plugin turns the
// timeline-semaphore feature on once its extension is enabled.

#include "CoreMinimal.h"
#include "Modules/ModuleManager.h"
#include "VulkanRHIBridge.h"
#include "VulkanRHIPrivate.h"

#include "MoqBridgeSettings.h"

class FMoqBridgeModule : public IModuleInterface
{
public:
  void StartupModule() override
  {
    // Only a simulator started for the bridge changes its device; stock
    // launches stay stock.
    FString Socket;
    if (!MoqBridge::GetSocket(Socket))
    {
      return;
    }
    VulkanRHIBridge::AddEnabledDeviceExtensionsAndLayers(
        {
            VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME,
            VK_KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION_NAME,
            VK_KHR_TIMELINE_SEMAPHORE_EXTENSION_NAME,
        },
        {});
  }
};

IMPLEMENT_MODULE(FMoqBridgeModule, MoqBridge)
