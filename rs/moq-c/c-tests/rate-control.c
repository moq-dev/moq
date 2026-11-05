// Exercise rate-control records and handle ownership across the generated C ABI.
#include <moq.h>
#include <assert.h>

int main(void) {
    moq_video_init video = {0};
    assert(video.lower == 0);
    moq_rate_policy policy = {.max_bps = 4000000, .lower_bps = 2000000, .has_lower = true};
    int32_t control = moq_rate_control_create(policy);
    assert(control > 0);
    moq_rate_target target = {0};
    assert(moq_rate_control_target((uint32_t)control, &target) == 0);
    assert(target.enabled && target.bitrate_bps == policy.max_bps);
    assert(moq_rate_control_close((uint32_t)control) == 0);
    assert(moq_rate_control_target((uint32_t)control, &target) < 0);
    return 0;
}
