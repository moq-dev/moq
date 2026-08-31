# [L] ROS 2 bridge

## Goal

A ROS 2 bridge sibling to the MAVLink one, carrying topics over the same two
delivery classes.

## Plan

### It must land in MCAP

Recordability is the axis this loses on if ignored. `rtsp_image_transport`
publishes a URL string, so rosbag2 records the URL and not the video; Foxglove's
`CompressedVideo` beat it despite banning B-frames and re-stapling parameter
sets to every keyframe, purely because it stays a recordable message.
Teleoperation video is training data, so a bridge that breaks the recording
loses regardless of latency.

### Why not DDS

Nobody runs RTPS over a WAN: discovery data grows quadratically, there is no
NAT traversal short of a port-forward rule per pair of communicating clients,
and reliable QoS on a lossy link produces latency spikes rather than delivery.
Every serious deployment already terminates DDS at the edge and
re-encapsulates. The competitors are the ones named in the questline README,
not the middleware.

### Sizing

Larger than the MAVLink bridge: type handling, QoS mapping, and the MCAP
requirement are each real work. Take it after the primitive has one integration
proving it, which is why it requires the MAVLink bridge rather than only the
crate.

## Required

- [Robot teleoperation primitive](/quest/m3/teleop/robot.md)
- [MAVLink bridge](/quest/m3/teleop/mavlink.md)
