# The published 0.9.16 image's cooked assets with the source-built executable,
# its crash-log symbols, and the GPU bridge library it loads.
# The config files carry the build's optional modules, which differ from the
# published image's: without them, the executable looks for plugins it lacks.
# Carla.uplugin lists the MoqBridge module.
FROM docker.io/carlasim/carla@sha256:aaf1df22702780ece072069e23d03c4879b002ae028c79744b09c4c7ddbae953

COPY --chown=carla:carla CarlaUE4-Linux-Shipping CarlaUE4-Linux-Shipping.sym libcarla_bridge.so /workspace/CarlaUE4/Binaries/Linux/
COPY --chown=carla:carla OptionalModules.ini /workspace/CarlaUE4/Config/
COPY --chown=carla:carla CarlaUE4.uproject /workspace/CarlaUE4/
COPY --chown=carla:carla CarlaTools.uplugin /workspace/CarlaUE4/Plugins/CarlaTools/
COPY --chown=carla:carla Carla.uplugin /workspace/CarlaUE4/Plugins/Carla/
