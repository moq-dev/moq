// Exercise rate-control records and handle ownership across the generated C ABI.
#include <moq.h>
#include <assert.h>

int main(void) {
    moq_video_init video = {0};
    assert(video.lower == 0);
    moq_rate_policy policy = {.max_bps = 4000000, .lower_bps = 2000000, .has_lower = true};
    // A control refuses an invalid reservation at setup.
    assert(moq_rate_control_create(0, policy) < 0);
    moq_rate_target target = {0};
    assert(moq_rate_control_target(0, &target) < 0);
    assert(moq_rate_control_update(0, &target) < 0);
    assert(moq_rate_control_close(0) < 0);
    return 0;
}
