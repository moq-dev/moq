// The CARLA demo's GPU video bridge, Unreal side: a module of the Carla plugin,
// copied into it by demo/carla/sim/patches/compile.sh.

using System.IO;
using UnrealBuildTool;

public class MoqBridge : ModuleRules
{
  public MoqBridge(ReadOnlyTargetRules Target) : base(Target)
  {
    PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;

    PublicDependencyModuleNames.AddRange(new string[] { "Core", "Engine" });
    PrivateDependencyModuleNames.AddRange(new string[] { "RHI", "RenderCore", "VulkanRHI" });

    // A capture records into Unreal's own Vulkan command buffers and reads its
    // layout tracking, which only the RHI's private headers declare. They link
    // because the Shipping target is monolithic.
    string VulkanRHI = Path.Combine(EngineDirectory, "Source", "Runtime", "VulkanRHI", "Private");
    PrivateIncludePaths.AddRange(new string[] {
      VulkanRHI,
      Path.Combine(VulkanRHI, Target.Platform.ToString()),
    });
    AddEngineThirdPartyPrivateStaticDependencies(Target, "Vulkan");
  }
}
