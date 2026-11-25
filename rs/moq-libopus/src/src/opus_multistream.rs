#[derive(Copy, Clone)]
#[repr(C)]
pub struct ChannelLayout {
    pub nb_channels: i32,
    pub nb_streams: i32,
    pub nb_coupled_streams: i32,
    pub mapping: [u8; 256],
}
pub unsafe fn validate_layout(layout: *const ChannelLayout) -> i32 {
    let mut i: i32 = 0;
    let mut max_channel: i32 = 0;
    max_channel = (*layout).nb_streams + (*layout).nb_coupled_streams;
    if max_channel > 255 {
        return 0;
    }
    i = 0;
    while i < (*layout).nb_channels {
        if (*layout).mapping[i as usize] as i32 >= max_channel
            && (*layout).mapping[i as usize] as i32 != 255
        {
            return 0;
        }
        i += 1;
    }
    return 1;
}
pub unsafe fn get_left_channel(layout: *const ChannelLayout, stream_id: i32, prev: i32) -> i32 {
    let mut i: i32 = 0;
    i = if prev < 0 { 0 } else { prev + 1 };
    while i < (*layout).nb_channels {
        if (*layout).mapping[i as usize] as i32 == stream_id * 2 {
            return i;
        }
        i += 1;
    }
    return -1;
}
pub unsafe fn get_right_channel(layout: *const ChannelLayout, stream_id: i32, prev: i32) -> i32 {
    let mut i: i32 = 0;
    i = if prev < 0 { 0 } else { prev + 1 };
    while i < (*layout).nb_channels {
        if (*layout).mapping[i as usize] as i32 == stream_id * 2 + 1 {
            return i;
        }
        i += 1;
    }
    return -1;
}
pub unsafe fn get_mono_channel(layout: *const ChannelLayout, stream_id: i32, prev: i32) -> i32 {
    let mut i: i32 = 0;
    i = if prev < 0 { 0 } else { prev + 1 };
    while i < (*layout).nb_channels {
        if (*layout).mapping[i as usize] as i32 == stream_id + (*layout).nb_coupled_streams {
            return i;
        }
        i += 1;
    }
    return -1;
}
