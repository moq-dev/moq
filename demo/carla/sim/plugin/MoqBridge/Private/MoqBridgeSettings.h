// The simulator's command-line options for the bridge.

#pragma once

#include "CoreMinimal.h"
#include "Misc/CommandLine.h"
#include "Misc/Parse.h"

namespace MoqBridge
{
  /// `-moq-bridge-socket=PATH`: where the bridge listens for the worker.
  /// Without it the simulator is stock and no camera can use the bridge.
  inline bool GetSocket(FString &Out)
  {
    return FParse::Value(FCommandLine::Get(), TEXT("moq-bridge-socket="), Out) && !Out.IsEmpty();
  }
}
