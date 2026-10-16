/*
 * The CARLA demo's GPU video bridge, as the CARLA plugin sees it.
 *
 * A source is one camera. The plugin allocates `slots` exportable Vulkan
 * images on Unreal's device, each a dedicated VK_IMAGE_TILING_OPTIMAL
 * allocation of the capture size in R8G8B8A8 or B8G8R8A8 UNORM with
 * VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT, plus one timeline semaphore
 * per image exported as an opaque fd and starting at 0. Per rendered frame,
 * on the RHI thread:
 *
 *   if (carla_bridge_source_wanted(source) &&
 *       carla_bridge_source_acquire(source, &slot, &ready) == 1) {
 *     record: barrier (after every earlier use of the image on this queue)
 *             -> copy the render target into image[slot]
 *             -> transition image[slot] to VK_IMAGE_LAYOUT_GENERAL
 *     vkQueueSubmit(..., signal timeline[slot] = ready)
 *     carla_bridge_source_submit(source, slot, frame, timestamp_us);
 *   }
 *
 * Nothing here blocks except carla_bridge_source_close. Functions returning
 * int report -1 on failure with a message in carla_bridge_error(). A producer
 * that cannot go on, whether for such an error or its own (a failed copy or
 * submission), reports it with carla_bridge_source_fail, which the worker
 * sees as the stream failing, then closes the source.
 *
 * This file has two identical copies: moq's
 * demo/carla/bridge/include/carla_bridge.h, which the bridge library
 * implements, and https://github.com/moq-dev/carla's include/carla_bridge.h,
 * which the plugin compiles against. Change both together; the simulator
 * image build refuses a mismatch.
 */

#ifndef CARLA_BRIDGE_H
#define CARLA_BRIDGE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CarlaBridgeSource CarlaBridgeSource;

#define CARLA_BRIDGE_RGBA 0u
#define CARLA_BRIDGE_BGRA 1u

/*
 * Register a camera of stream `stream` (the worker's broadcast, e.g.
 * "truck1/front"). The first call in a process starts the bridge listening on
 * `socket`, which fails while another process's bridge holds that path; later
 * calls must name the same path. `device_uuid` and `driver_uuid` are the
 * 16-byte VkPhysicalDeviceIDProperties::deviceUUID and ::driverUUID of the
 * rendering device; every camera of a stream renders on one device. Returns
 * NULL on failure.
 */
CarlaBridgeSource *carla_bridge_source_open(const char *socket, const char *stream, const uint8_t *device_uuid,
					    const uint8_t *driver_uuid, uint32_t width, uint32_t height, uint32_t channels,
					    uint32_t slots);

/*
 * Import slot `slot`'s image memory and timeline semaphore, each exported as an
 * opaque fd. Both descriptors are consumed whatever the result.
 * `allocation_size` is the whole VkDeviceMemory size and `memory_type` the
 * VkMemoryAllocateInfo::memoryTypeIndex it was allocated from; an opaque
 * import must repeat both.
 */
int carla_bridge_source_import(CarlaBridgeSource *source, uint32_t slot, int memory_fd, int timeline_fd,
			       uint64_t allocation_size, uint32_t memory_type);

/* Whether the stream wants this camera's pictures now: render only if so. */
bool carla_bridge_source_wanted(const CarlaBridgeSource *source);

/*
 * Take an idle slot. Returns 1 with the slot and the timeline value the copy's
 * submission must signal, 0 when every slot is busy (drop this picture), or -1.
 */
int carla_bridge_source_acquire(CarlaBridgeSource *source, uint32_t *slot, uint64_t *ready);

/*
 * Hand an acquired slot to the encoder once its submission, including the
 * `ready` signal, is queued. `frame` is CARLA's frame counter and
 * `timestamp_us` the simulation time of the picture.
 */
int carla_bridge_source_submit(CarlaBridgeSource *source, uint32_t slot, uint64_t frame, uint64_t timestamp_us);

/*
 * The producer can no longer feed this camera, for `message`. The stream stops
 * wanting it and fails the worker's session for the stream, and every session
 * opened on it until another camera registers; close the source next. Never
 * blocks.
 */
int carla_bridge_source_fail(CarlaBridgeSource *source, const char *message);

/* Return an acquired slot without submitting it, when recording failed. */
int carla_bridge_source_cancel(CarlaBridgeSource *source, uint32_t slot);

/*
 * Stop the camera and free `source`. Blocks until the GPU has finished with
 * every slot, after which the images and semaphores may be destroyed.
 */
void carla_bridge_source_close(CarlaBridgeSource *source);

/* The calling thread's last error, valid until its next bridge call. */
const char *carla_bridge_error(void);

#ifdef __cplusplus
}
#endif

#endif
