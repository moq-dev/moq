// ignore_for_file: unused_import, type=lint

library moq_ffi;

import "dart:async";
import "dart:convert";
import "dart:ffi";
import "dart:io" show Platform, File, Directory;
import "dart:isolate";
import "dart:typed_data";
import "package:ffi/ffi.dart";
import "uniffi_runtime.dart";
export "uniffi_runtime.dart";

class MoqFetchGroupOptions {
  final int priority;
  MoqFetchGroupOptions({this.priority = 0});
}

class FfiConverterMoqFetchGroupOptions {
  static MoqFetchGroupOptions lift(RustBuffer buf) {
    return FfiConverterMoqFetchGroupOptions.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqFetchGroupOptions> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final priority_lifted = FfiConverterUInt8.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final priority = priority_lifted.value;
    new_offset += priority_lifted.bytesRead;
    return LiftRetVal(
      MoqFetchGroupOptions(priority: priority),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqFetchGroupOptions value) {
    final total_length = FfiConverterUInt8.allocationSize(value.priority) + 0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqFetchGroupOptions value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt8.write(
      value.priority,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqFetchGroupOptions value) {
    return FfiConverterUInt8.allocationSize(value.priority) + 0;
  }
}

class MoqSubscription {
  final int priority;
  final int maxAgeMs;
  final int? groupStart;
  final int? groupEnd;
  MoqSubscription({
    this.priority = 0,
    this.maxAgeMs = 0,
    this.groupStart = null,
    this.groupEnd = null,
  });
}

class FfiConverterMoqSubscription {
  static MoqSubscription lift(RustBuffer buf) {
    return FfiConverterMoqSubscription.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqSubscription> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final priority_lifted = FfiConverterUInt8.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final priority = priority_lifted.value;
    new_offset += priority_lifted.bytesRead;
    final maxAgeMs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final maxAgeMs = maxAgeMs_lifted.value;
    new_offset += maxAgeMs_lifted.bytesRead;
    final groupStart_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final groupStart = groupStart_lifted.value;
    new_offset += groupStart_lifted.bytesRead;
    final groupEnd_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final groupEnd = groupEnd_lifted.value;
    new_offset += groupEnd_lifted.bytesRead;
    return LiftRetVal(
      MoqSubscription(
        priority: priority,
        maxAgeMs: maxAgeMs,
        groupStart: groupStart,
        groupEnd: groupEnd,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqSubscription value) {
    final total_length =
        FfiConverterUInt8.allocationSize(value.priority) +
        FfiConverterUInt64.allocationSize(value.maxAgeMs) +
        FfiConverterOptionalUInt64.allocationSize(value.groupStart) +
        FfiConverterOptionalUInt64.allocationSize(value.groupEnd) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqSubscription value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt8.write(
      value.priority,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.maxAgeMs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.groupStart,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.groupEnd,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqSubscription value) {
    return FfiConverterUInt8.allocationSize(value.priority) +
        FfiConverterUInt64.allocationSize(value.maxAgeMs) +
        FfiConverterOptionalUInt64.allocationSize(value.groupStart) +
        FfiConverterOptionalUInt64.allocationSize(value.groupEnd) +
        0;
  }
}

class MoqJsonSnapshotConfig {
  final int deltaRatio;
  final bool compression;
  MoqJsonSnapshotConfig({this.deltaRatio = 8, this.compression = false});
}

class FfiConverterMoqJsonSnapshotConfig {
  static MoqJsonSnapshotConfig lift(RustBuffer buf) {
    return FfiConverterMoqJsonSnapshotConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqJsonSnapshotConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final deltaRatio_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final deltaRatio = deltaRatio_lifted.value;
    new_offset += deltaRatio_lifted.bytesRead;
    final compression_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final compression = compression_lifted.value;
    new_offset += compression_lifted.bytesRead;
    return LiftRetVal(
      MoqJsonSnapshotConfig(deltaRatio: deltaRatio, compression: compression),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqJsonSnapshotConfig value) {
    final total_length =
        FfiConverterUInt32.allocationSize(value.deltaRatio) +
        FfiConverterBool.allocationSize(value.compression) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqJsonSnapshotConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt32.write(
      value.deltaRatio,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterBool.write(
      value.compression,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqJsonSnapshotConfig value) {
    return FfiConverterUInt32.allocationSize(value.deltaRatio) +
        FfiConverterBool.allocationSize(value.compression) +
        0;
  }
}

class MoqJsonStreamConfig {
  final bool compression;
  MoqJsonStreamConfig({this.compression = false});
}

class FfiConverterMoqJsonStreamConfig {
  static MoqJsonStreamConfig lift(RustBuffer buf) {
    return FfiConverterMoqJsonStreamConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqJsonStreamConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final compression_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final compression = compression_lifted.value;
    new_offset += compression_lifted.bytesRead;
    return LiftRetVal(
      MoqJsonStreamConfig(compression: compression),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqJsonStreamConfig value) {
    final total_length = FfiConverterBool.allocationSize(value.compression) + 0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqJsonStreamConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterBool.write(
      value.compression,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqJsonStreamConfig value) {
    return FfiConverterBool.allocationSize(value.compression) + 0;
  }
}

class MoqAudio {
  final String? label;
  final String? broadcast;
  final String codec;
  final Uint8List? description;
  final int sampleRate;
  final int channelCount;
  final int? bitrate;
  final MoqContainer container;
  MoqAudio({
    this.label = null,
    this.broadcast = null,
    required this.codec,
    this.description,
    required this.sampleRate,
    required this.channelCount,
    this.bitrate,
    required this.container,
  });
}

class FfiConverterMoqAudio {
  static MoqAudio lift(RustBuffer buf) {
    return FfiConverterMoqAudio.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudio> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final label_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final label = label_lifted.value;
    new_offset += label_lifted.bytesRead;
    final broadcast_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final broadcast = broadcast_lifted.value;
    new_offset += broadcast_lifted.bytesRead;
    final codec_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final codec = codec_lifted.value;
    new_offset += codec_lifted.bytesRead;
    final description_lifted = FfiConverterOptionalUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final description = description_lifted.value;
    new_offset += description_lifted.bytesRead;
    final sampleRate_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final sampleRate = sampleRate_lifted.value;
    new_offset += sampleRate_lifted.bytesRead;
    final channelCount_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final channelCount = channelCount_lifted.value;
    new_offset += channelCount_lifted.bytesRead;
    final bitrate_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bitrate = bitrate_lifted.value;
    new_offset += bitrate_lifted.bytesRead;
    final container_lifted = FfiConverterMoqContainer.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final container = container_lifted.value;
    new_offset += container_lifted.bytesRead;
    return LiftRetVal(
      MoqAudio(
        label: label,
        broadcast: broadcast,
        codec: codec,
        description: description,
        sampleRate: sampleRate,
        channelCount: channelCount,
        bitrate: bitrate,
        container: container,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudio value) {
    final total_length =
        FfiConverterOptionalString.allocationSize(value.label) +
        FfiConverterOptionalString.allocationSize(value.broadcast) +
        FfiConverterString.allocationSize(value.codec) +
        FfiConverterOptionalUint8List.allocationSize(value.description) +
        FfiConverterUInt32.allocationSize(value.sampleRate) +
        FfiConverterUInt32.allocationSize(value.channelCount) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterMoqContainer.allocationSize(value.container) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudio value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalString.write(
      value.label,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.broadcast,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterString.write(
      value.codec,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUint8List.write(
      value.description,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.sampleRate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.channelCount,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.bitrate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqContainer.write(
      value.container,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudio value) {
    return FfiConverterOptionalString.allocationSize(value.label) +
        FfiConverterOptionalString.allocationSize(value.broadcast) +
        FfiConverterString.allocationSize(value.codec) +
        FfiConverterOptionalUint8List.allocationSize(value.description) +
        FfiConverterUInt32.allocationSize(value.sampleRate) +
        FfiConverterUInt32.allocationSize(value.channelCount) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterMoqContainer.allocationSize(value.container) +
        0;
  }
}

class MoqAudioInit {
  final MoqAudioFormat format;
  final Uint8List data;
  final String? label;
  MoqAudioInit({required this.format, required this.data, this.label = null});
}

class FfiConverterMoqAudioInit {
  static MoqAudioInit lift(RustBuffer buf) {
    return FfiConverterMoqAudioInit.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioInit> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final format_lifted = FfiConverterMoqAudioFormat.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final format = format_lifted.value;
    new_offset += format_lifted.bytesRead;
    final data_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final data = data_lifted.value;
    new_offset += data_lifted.bytesRead;
    final label_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final label = label_lifted.value;
    new_offset += label_lifted.bytesRead;
    return LiftRetVal(
      MoqAudioInit(format: format, data: data, label: label),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudioInit value) {
    final total_length =
        FfiConverterMoqAudioFormat.allocationSize(value.format) +
        FfiConverterUint8List.allocationSize(value.data) +
        FfiConverterOptionalString.allocationSize(value.label) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudioInit value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqAudioFormat.write(
      value.format,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUint8List.write(
      value.data,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.label,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudioInit value) {
    return FfiConverterMoqAudioFormat.allocationSize(value.format) +
        FfiConverterUint8List.allocationSize(value.data) +
        FfiConverterOptionalString.allocationSize(value.label) +
        0;
  }
}

class MoqCatalog {
  final Map<String, MoqVideo> video;
  final Map<String, MoqAudio> audio;
  final MoqDimensions? display;
  final double? rotation;
  final bool? flip;
  final Map<String, String> sections;
  MoqCatalog({
    required this.video,
    required this.audio,
    this.display,
    this.rotation,
    this.flip,
    required this.sections,
  });
}

class FfiConverterMoqCatalog {
  static MoqCatalog lift(RustBuffer buf) {
    return FfiConverterMoqCatalog.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqCatalog> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final video_lifted = FfiConverterMapStringToMoqVideo.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final video = video_lifted.value;
    new_offset += video_lifted.bytesRead;
    final audio_lifted = FfiConverterMapStringToMoqAudio.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final audio = audio_lifted.value;
    new_offset += audio_lifted.bytesRead;
    final display_lifted = FfiConverterOptionalMoqDimensions.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final display = display_lifted.value;
    new_offset += display_lifted.bytesRead;
    final rotation_lifted = FfiConverterOptionalDouble64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final rotation = rotation_lifted.value;
    new_offset += rotation_lifted.bytesRead;
    final flip_lifted = FfiConverterOptionalBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final flip = flip_lifted.value;
    new_offset += flip_lifted.bytesRead;
    final sections_lifted = FfiConverterMapStringToString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final sections = sections_lifted.value;
    new_offset += sections_lifted.bytesRead;
    return LiftRetVal(
      MoqCatalog(
        video: video,
        audio: audio,
        display: display,
        rotation: rotation,
        flip: flip,
        sections: sections,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqCatalog value) {
    final total_length =
        FfiConverterMapStringToMoqVideo.allocationSize(value.video) +
        FfiConverterMapStringToMoqAudio.allocationSize(value.audio) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.display) +
        FfiConverterOptionalDouble64.allocationSize(value.rotation) +
        FfiConverterOptionalBool.allocationSize(value.flip) +
        FfiConverterMapStringToString.allocationSize(value.sections) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqCatalog value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMapStringToMoqVideo.write(
      value.video,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMapStringToMoqAudio.write(
      value.audio,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqDimensions.write(
      value.display,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDouble64.write(
      value.rotation,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalBool.write(
      value.flip,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMapStringToString.write(
      value.sections,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqCatalog value) {
    return FfiConverterMapStringToMoqVideo.allocationSize(value.video) +
        FfiConverterMapStringToMoqAudio.allocationSize(value.audio) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.display) +
        FfiConverterOptionalDouble64.allocationSize(value.rotation) +
        FfiConverterOptionalBool.allocationSize(value.flip) +
        FfiConverterMapStringToString.allocationSize(value.sections) +
        0;
  }
}

class MoqContainerInit {
  final MoqContainerFormat format;
  final Uint8List data;
  MoqContainerInit({required this.format, required this.data});
}

class FfiConverterMoqContainerInit {
  static MoqContainerInit lift(RustBuffer buf) {
    return FfiConverterMoqContainerInit.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqContainerInit> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final format_lifted = FfiConverterMoqContainerFormat.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final format = format_lifted.value;
    new_offset += format_lifted.bytesRead;
    final data_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final data = data_lifted.value;
    new_offset += data_lifted.bytesRead;
    return LiftRetVal(
      MoqContainerInit(format: format, data: data),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqContainerInit value) {
    final total_length =
        FfiConverterMoqContainerFormat.allocationSize(value.format) +
        FfiConverterUint8List.allocationSize(value.data) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqContainerInit value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqContainerFormat.write(
      value.format,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUint8List.write(
      value.data,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqContainerInit value) {
    return FfiConverterMoqContainerFormat.allocationSize(value.format) +
        FfiConverterUint8List.allocationSize(value.data) +
        0;
  }
}

class MoqDatagram {
  final int sequence;
  final int timestampUs;
  final Uint8List payload;
  MoqDatagram({this.sequence = 0, this.timestampUs = 0, required this.payload});
}

class FfiConverterMoqDatagram {
  static MoqDatagram lift(RustBuffer buf) {
    return FfiConverterMoqDatagram.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqDatagram> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final sequence_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final sequence = sequence_lifted.value;
    new_offset += sequence_lifted.bytesRead;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    final payload_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final payload = payload_lifted.value;
    new_offset += payload_lifted.bytesRead;
    return LiftRetVal(
      MoqDatagram(
        sequence: sequence,
        timestampUs: timestampUs,
        payload: payload,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqDatagram value) {
    final total_length =
        FfiConverterUInt64.allocationSize(value.sequence) +
        FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterUint8List.allocationSize(value.payload) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqDatagram value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt64.write(
      value.sequence,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUint8List.write(
      value.payload,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqDatagram value) {
    return FfiConverterUInt64.allocationSize(value.sequence) +
        FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterUint8List.allocationSize(value.payload) +
        0;
  }
}

class MoqDimensions {
  final int width;
  final int height;
  MoqDimensions({required this.width, required this.height});
}

class FfiConverterMoqDimensions {
  static MoqDimensions lift(RustBuffer buf) {
    return FfiConverterMoqDimensions.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqDimensions> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final width_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final width = width_lifted.value;
    new_offset += width_lifted.bytesRead;
    final height_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final height = height_lifted.value;
    new_offset += height_lifted.bytesRead;
    return LiftRetVal(
      MoqDimensions(width: width, height: height),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqDimensions value) {
    final total_length =
        FfiConverterUInt32.allocationSize(value.width) +
        FfiConverterUInt32.allocationSize(value.height) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqDimensions value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt32.write(
      value.width,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.height,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqDimensions value) {
    return FfiConverterUInt32.allocationSize(value.width) +
        FfiConverterUInt32.allocationSize(value.height) +
        0;
  }
}

class MoqFrame {
  final Uint8List payload;
  final int timestampUs;
  MoqFrame({required this.payload, this.timestampUs = 0});
}

class FfiConverterMoqFrame {
  static MoqFrame lift(RustBuffer buf) {
    return FfiConverterMoqFrame.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqFrame> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final payload_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final payload = payload_lifted.value;
    new_offset += payload_lifted.bytesRead;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    return LiftRetVal(
      MoqFrame(payload: payload, timestampUs: timestampUs),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqFrame value) {
    final total_length =
        FfiConverterUint8List.allocationSize(value.payload) +
        FfiConverterUInt64.allocationSize(value.timestampUs) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqFrame value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUint8List.write(
      value.payload,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqFrame value) {
    return FfiConverterUint8List.allocationSize(value.payload) +
        FfiConverterUInt64.allocationSize(value.timestampUs) +
        0;
  }
}

class MoqMediaFrame {
  final Uint8List payload;
  final int timestampUs;
  final bool keyframe;
  MoqMediaFrame({
    required this.payload,
    required this.timestampUs,
    required this.keyframe,
  });
}

class FfiConverterMoqMediaFrame {
  static MoqMediaFrame lift(RustBuffer buf) {
    return FfiConverterMoqMediaFrame.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqMediaFrame> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final payload_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final payload = payload_lifted.value;
    new_offset += payload_lifted.bytesRead;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    final keyframe_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final keyframe = keyframe_lifted.value;
    new_offset += keyframe_lifted.bytesRead;
    return LiftRetVal(
      MoqMediaFrame(
        payload: payload,
        timestampUs: timestampUs,
        keyframe: keyframe,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqMediaFrame value) {
    final total_length =
        FfiConverterUint8List.allocationSize(value.payload) +
        FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterBool.allocationSize(value.keyframe) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqMediaFrame value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUint8List.write(
      value.payload,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterBool.write(
      value.keyframe,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqMediaFrame value) {
    return FfiConverterUint8List.allocationSize(value.payload) +
        FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterBool.allocationSize(value.keyframe) +
        0;
  }
}

class MoqVideo {
  final String? label;
  final String? broadcast;
  final String codec;
  final Uint8List? description;
  final MoqDimensions? coded;
  final MoqDimensions? displayAspect;
  final int? bitrate;
  final bool stalled;
  final double? framerate;
  final MoqContainer container;
  MoqVideo({
    this.label = null,
    this.broadcast = null,
    required this.codec,
    this.description,
    this.coded,
    this.displayAspect,
    this.bitrate,
    this.stalled = false,
    this.framerate,
    required this.container,
  });
}

class FfiConverterMoqVideo {
  static MoqVideo lift(RustBuffer buf) {
    return FfiConverterMoqVideo.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideo> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final label_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final label = label_lifted.value;
    new_offset += label_lifted.bytesRead;
    final broadcast_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final broadcast = broadcast_lifted.value;
    new_offset += broadcast_lifted.bytesRead;
    final codec_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final codec = codec_lifted.value;
    new_offset += codec_lifted.bytesRead;
    final description_lifted = FfiConverterOptionalUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final description = description_lifted.value;
    new_offset += description_lifted.bytesRead;
    final coded_lifted = FfiConverterOptionalMoqDimensions.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final coded = coded_lifted.value;
    new_offset += coded_lifted.bytesRead;
    final displayAspect_lifted = FfiConverterOptionalMoqDimensions.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final displayAspect = displayAspect_lifted.value;
    new_offset += displayAspect_lifted.bytesRead;
    final bitrate_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bitrate = bitrate_lifted.value;
    new_offset += bitrate_lifted.bytesRead;
    final stalled_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final stalled = stalled_lifted.value;
    new_offset += stalled_lifted.bytesRead;
    final framerate_lifted = FfiConverterOptionalDouble64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final framerate = framerate_lifted.value;
    new_offset += framerate_lifted.bytesRead;
    final container_lifted = FfiConverterMoqContainer.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final container = container_lifted.value;
    new_offset += container_lifted.bytesRead;
    return LiftRetVal(
      MoqVideo(
        label: label,
        broadcast: broadcast,
        codec: codec,
        description: description,
        coded: coded,
        displayAspect: displayAspect,
        bitrate: bitrate,
        stalled: stalled,
        framerate: framerate,
        container: container,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideo value) {
    final total_length =
        FfiConverterOptionalString.allocationSize(value.label) +
        FfiConverterOptionalString.allocationSize(value.broadcast) +
        FfiConverterString.allocationSize(value.codec) +
        FfiConverterOptionalUint8List.allocationSize(value.description) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.coded) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.displayAspect) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterBool.allocationSize(value.stalled) +
        FfiConverterOptionalDouble64.allocationSize(value.framerate) +
        FfiConverterMoqContainer.allocationSize(value.container) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideo value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalString.write(
      value.label,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.broadcast,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterString.write(
      value.codec,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUint8List.write(
      value.description,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqDimensions.write(
      value.coded,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqDimensions.write(
      value.displayAspect,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.bitrate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterBool.write(
      value.stalled,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDouble64.write(
      value.framerate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqContainer.write(
      value.container,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideo value) {
    return FfiConverterOptionalString.allocationSize(value.label) +
        FfiConverterOptionalString.allocationSize(value.broadcast) +
        FfiConverterString.allocationSize(value.codec) +
        FfiConverterOptionalUint8List.allocationSize(value.description) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.coded) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.displayAspect) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterBool.allocationSize(value.stalled) +
        FfiConverterOptionalDouble64.allocationSize(value.framerate) +
        FfiConverterMoqContainer.allocationSize(value.container) +
        0;
  }
}

class MoqVideoHint {
  final MoqDimensions? coded;
  final MoqDimensions? displayAspect;
  final int? bitrate;
  final double? framerate;
  final bool? optimizeForLatency;
  MoqVideoHint({
    this.coded = null,
    this.displayAspect = null,
    this.bitrate = null,
    this.framerate = null,
    this.optimizeForLatency = null,
  });
}

class FfiConverterMoqVideoHint {
  static MoqVideoHint lift(RustBuffer buf) {
    return FfiConverterMoqVideoHint.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoHint> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final coded_lifted = FfiConverterOptionalMoqDimensions.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final coded = coded_lifted.value;
    new_offset += coded_lifted.bytesRead;
    final displayAspect_lifted = FfiConverterOptionalMoqDimensions.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final displayAspect = displayAspect_lifted.value;
    new_offset += displayAspect_lifted.bytesRead;
    final bitrate_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bitrate = bitrate_lifted.value;
    new_offset += bitrate_lifted.bytesRead;
    final framerate_lifted = FfiConverterOptionalDouble64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final framerate = framerate_lifted.value;
    new_offset += framerate_lifted.bytesRead;
    final optimizeForLatency_lifted = FfiConverterOptionalBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final optimizeForLatency = optimizeForLatency_lifted.value;
    new_offset += optimizeForLatency_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoHint(
        coded: coded,
        displayAspect: displayAspect,
        bitrate: bitrate,
        framerate: framerate,
        optimizeForLatency: optimizeForLatency,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoHint value) {
    final total_length =
        FfiConverterOptionalMoqDimensions.allocationSize(value.coded) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.displayAspect) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterOptionalDouble64.allocationSize(value.framerate) +
        FfiConverterOptionalBool.allocationSize(value.optimizeForLatency) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoHint value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalMoqDimensions.write(
      value.coded,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqDimensions.write(
      value.displayAspect,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.bitrate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDouble64.write(
      value.framerate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalBool.write(
      value.optimizeForLatency,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoHint value) {
    return FfiConverterOptionalMoqDimensions.allocationSize(value.coded) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.displayAspect) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterOptionalDouble64.allocationSize(value.framerate) +
        FfiConverterOptionalBool.allocationSize(value.optimizeForLatency) +
        0;
  }
}

class MoqVideoInit {
  final MoqVideoFormat format;
  final Uint8List data;
  final String? label;
  final MoqVideoHint? hint;
  MoqVideoInit({
    required this.format,
    required this.data,
    this.label = null,
    this.hint = null,
  });
}

class FfiConverterMoqVideoInit {
  static MoqVideoInit lift(RustBuffer buf) {
    return FfiConverterMoqVideoInit.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoInit> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final format_lifted = FfiConverterMoqVideoFormat.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final format = format_lifted.value;
    new_offset += format_lifted.bytesRead;
    final data_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final data = data_lifted.value;
    new_offset += data_lifted.bytesRead;
    final label_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final label = label_lifted.value;
    new_offset += label_lifted.bytesRead;
    final hint_lifted = FfiConverterOptionalMoqVideoHint.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final hint = hint_lifted.value;
    new_offset += hint_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoInit(format: format, data: data, label: label, hint: hint),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoInit value) {
    final total_length =
        FfiConverterMoqVideoFormat.allocationSize(value.format) +
        FfiConverterUint8List.allocationSize(value.data) +
        FfiConverterOptionalString.allocationSize(value.label) +
        FfiConverterOptionalMoqVideoHint.allocationSize(value.hint) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoInit value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqVideoFormat.write(
      value.format,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUint8List.write(
      value.data,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.label,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqVideoHint.write(
      value.hint,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoInit value) {
    return FfiConverterMoqVideoFormat.allocationSize(value.format) +
        FfiConverterUint8List.allocationSize(value.data) +
        FfiConverterOptionalString.allocationSize(value.label) +
        FfiConverterOptionalMoqVideoHint.allocationSize(value.hint) +
        0;
  }
}

class MoqVideoProperties {
  final MoqDimensions? display;
  final double? rotation;
  final bool? flip;
  MoqVideoProperties({
    this.display = null,
    this.rotation = null,
    this.flip = null,
  });
}

class FfiConverterMoqVideoProperties {
  static MoqVideoProperties lift(RustBuffer buf) {
    return FfiConverterMoqVideoProperties.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoProperties> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final display_lifted = FfiConverterOptionalMoqDimensions.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final display = display_lifted.value;
    new_offset += display_lifted.bytesRead;
    final rotation_lifted = FfiConverterOptionalDouble64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final rotation = rotation_lifted.value;
    new_offset += rotation_lifted.bytesRead;
    final flip_lifted = FfiConverterOptionalBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final flip = flip_lifted.value;
    new_offset += flip_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoProperties(display: display, rotation: rotation, flip: flip),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoProperties value) {
    final total_length =
        FfiConverterOptionalMoqDimensions.allocationSize(value.display) +
        FfiConverterOptionalDouble64.allocationSize(value.rotation) +
        FfiConverterOptionalBool.allocationSize(value.flip) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoProperties value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalMoqDimensions.write(
      value.display,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDouble64.write(
      value.rotation,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalBool.write(
      value.flip,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoProperties value) {
    return FfiConverterOptionalMoqDimensions.allocationSize(value.display) +
        FfiConverterOptionalDouble64.allocationSize(value.rotation) +
        FfiConverterOptionalBool.allocationSize(value.flip) +
        0;
  }
}

class MoqOriginOptions {
  final int? cacheCapacityBytes;
  MoqOriginOptions({this.cacheCapacityBytes = null});
}

class FfiConverterMoqOriginOptions {
  static MoqOriginOptions lift(RustBuffer buf) {
    return FfiConverterMoqOriginOptions.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqOriginOptions> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final cacheCapacityBytes_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final cacheCapacityBytes = cacheCapacityBytes_lifted.value;
    new_offset += cacheCapacityBytes_lifted.bytesRead;
    return LiftRetVal(
      MoqOriginOptions(cacheCapacityBytes: cacheCapacityBytes),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqOriginOptions value) {
    final total_length =
        FfiConverterOptionalUInt64.allocationSize(value.cacheCapacityBytes) + 0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqOriginOptions value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalUInt64.write(
      value.cacheCapacityBytes,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqOriginOptions value) {
    return FfiConverterOptionalUInt64.allocationSize(value.cacheCapacityBytes) +
        0;
  }
}

class MoqRoute {
  final List<int> hops;
  final int cost;
  MoqRoute({this.hops = const [], this.cost = 0});
}

class FfiConverterMoqRoute {
  static MoqRoute lift(RustBuffer buf) {
    return FfiConverterMoqRoute.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqRoute> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final hops_lifted = FfiConverterSequenceUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final hops = hops_lifted.value;
    new_offset += hops_lifted.bytesRead;
    final cost_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final cost = cost_lifted.value;
    new_offset += cost_lifted.bytesRead;
    return LiftRetVal(
      MoqRoute(hops: hops, cost: cost),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqRoute value) {
    final total_length =
        FfiConverterSequenceUInt64.allocationSize(value.hops) +
        FfiConverterUInt64.allocationSize(value.cost) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqRoute value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterSequenceUInt64.write(
      value.hops,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.cost,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqRoute value) {
    return FfiConverterSequenceUInt64.allocationSize(value.hops) +
        FfiConverterUInt64.allocationSize(value.cost) +
        0;
  }
}

class MoqTrackInfo {
  final int priority;
  final int? maxAgeMs;
  final int? timescale;
  MoqTrackInfo({
    this.priority = 0,
    this.maxAgeMs = null,
    this.timescale = null,
  });
}

class FfiConverterMoqTrackInfo {
  static MoqTrackInfo lift(RustBuffer buf) {
    return FfiConverterMoqTrackInfo.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqTrackInfo> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final priority_lifted = FfiConverterUInt8.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final priority = priority_lifted.value;
    new_offset += priority_lifted.bytesRead;
    final maxAgeMs_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final maxAgeMs = maxAgeMs_lifted.value;
    new_offset += maxAgeMs_lifted.bytesRead;
    final timescale_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timescale = timescale_lifted.value;
    new_offset += timescale_lifted.bytesRead;
    return LiftRetVal(
      MoqTrackInfo(
        priority: priority,
        maxAgeMs: maxAgeMs,
        timescale: timescale,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqTrackInfo value) {
    final total_length =
        FfiConverterUInt8.allocationSize(value.priority) +
        FfiConverterOptionalUInt64.allocationSize(value.maxAgeMs) +
        FfiConverterOptionalUInt64.allocationSize(value.timescale) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqTrackInfo value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt8.write(
      value.priority,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.maxAgeMs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.timescale,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqTrackInfo value) {
    return FfiConverterUInt8.allocationSize(value.priority) +
        FfiConverterOptionalUInt64.allocationSize(value.maxAgeMs) +
        FfiConverterOptionalUInt64.allocationSize(value.timescale) +
        0;
  }
}

class MoqBackoff {
  final int initialMs;
  final int multiplier;
  final int maxMs;
  final int timeoutMs;
  MoqBackoff({
    this.initialMs = 1000,
    this.multiplier = 2,
    this.maxMs = 30000,
    this.timeoutMs = 300000,
  });
}

class FfiConverterMoqBackoff {
  static MoqBackoff lift(RustBuffer buf) {
    return FfiConverterMoqBackoff.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqBackoff> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final initialMs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final initialMs = initialMs_lifted.value;
    new_offset += initialMs_lifted.bytesRead;
    final multiplier_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final multiplier = multiplier_lifted.value;
    new_offset += multiplier_lifted.bytesRead;
    final maxMs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final maxMs = maxMs_lifted.value;
    new_offset += maxMs_lifted.bytesRead;
    final timeoutMs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timeoutMs = timeoutMs_lifted.value;
    new_offset += timeoutMs_lifted.bytesRead;
    return LiftRetVal(
      MoqBackoff(
        initialMs: initialMs,
        multiplier: multiplier,
        maxMs: maxMs,
        timeoutMs: timeoutMs,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqBackoff value) {
    final total_length =
        FfiConverterUInt64.allocationSize(value.initialMs) +
        FfiConverterUInt32.allocationSize(value.multiplier) +
        FfiConverterUInt64.allocationSize(value.maxMs) +
        FfiConverterUInt64.allocationSize(value.timeoutMs) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqBackoff value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt64.write(
      value.initialMs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.multiplier,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.maxMs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.timeoutMs,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqBackoff value) {
    return FfiConverterUInt64.allocationSize(value.initialMs) +
        FfiConverterUInt32.allocationSize(value.multiplier) +
        FfiConverterUInt64.allocationSize(value.maxMs) +
        FfiConverterUInt64.allocationSize(value.timeoutMs) +
        0;
  }
}

class MoqConnectionStats {
  final int? rttUs;
  final int? sendRateBps;
  final int? recvRateBps;
  final int? bytesSent;
  final int? bytesReceived;
  final int? bytesLost;
  final int? packetsSent;
  final int? packetsReceived;
  final int? packetsLost;
  MoqConnectionStats({
    this.rttUs,
    this.sendRateBps,
    this.recvRateBps,
    this.bytesSent,
    this.bytesReceived,
    this.bytesLost,
    this.packetsSent,
    this.packetsReceived,
    this.packetsLost,
  });
}

class FfiConverterMoqConnectionStats {
  static MoqConnectionStats lift(RustBuffer buf) {
    return FfiConverterMoqConnectionStats.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqConnectionStats> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final rttUs_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final rttUs = rttUs_lifted.value;
    new_offset += rttUs_lifted.bytesRead;
    final sendRateBps_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final sendRateBps = sendRateBps_lifted.value;
    new_offset += sendRateBps_lifted.bytesRead;
    final recvRateBps_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final recvRateBps = recvRateBps_lifted.value;
    new_offset += recvRateBps_lifted.bytesRead;
    final bytesSent_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bytesSent = bytesSent_lifted.value;
    new_offset += bytesSent_lifted.bytesRead;
    final bytesReceived_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bytesReceived = bytesReceived_lifted.value;
    new_offset += bytesReceived_lifted.bytesRead;
    final bytesLost_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bytesLost = bytesLost_lifted.value;
    new_offset += bytesLost_lifted.bytesRead;
    final packetsSent_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final packetsSent = packetsSent_lifted.value;
    new_offset += packetsSent_lifted.bytesRead;
    final packetsReceived_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final packetsReceived = packetsReceived_lifted.value;
    new_offset += packetsReceived_lifted.bytesRead;
    final packetsLost_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final packetsLost = packetsLost_lifted.value;
    new_offset += packetsLost_lifted.bytesRead;
    return LiftRetVal(
      MoqConnectionStats(
        rttUs: rttUs,
        sendRateBps: sendRateBps,
        recvRateBps: recvRateBps,
        bytesSent: bytesSent,
        bytesReceived: bytesReceived,
        bytesLost: bytesLost,
        packetsSent: packetsSent,
        packetsReceived: packetsReceived,
        packetsLost: packetsLost,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqConnectionStats value) {
    final total_length =
        FfiConverterOptionalUInt64.allocationSize(value.rttUs) +
        FfiConverterOptionalUInt64.allocationSize(value.sendRateBps) +
        FfiConverterOptionalUInt64.allocationSize(value.recvRateBps) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesSent) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesReceived) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesLost) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsSent) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsReceived) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsLost) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqConnectionStats value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalUInt64.write(
      value.rttUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.sendRateBps,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.recvRateBps,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.bytesSent,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.bytesReceived,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.bytesLost,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.packetsSent,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.packetsReceived,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.packetsLost,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqConnectionStats value) {
    return FfiConverterOptionalUInt64.allocationSize(value.rttUs) +
        FfiConverterOptionalUInt64.allocationSize(value.sendRateBps) +
        FfiConverterOptionalUInt64.allocationSize(value.recvRateBps) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesSent) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesReceived) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesLost) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsSent) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsReceived) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsLost) +
        0;
  }
}

enum MoqException implements Exception {
  protocol,
  media,
  mux,
  jsonTrack,
  url,
  timeOverflow,
  logLevel,
  task,
  json,
  cancelled,
  closed,
  connect,
  bind,
  reject,
  alreadyResponded,
  codec,
  unauthorized,
  forbidden,
  notFound,
  unsupported,
  alreadyCommitted,
  invalidRoute,
  unresolvableBroadcast,
  log,
}

class FfiConverterMoqException {
  static LiftRetVal<MoqException> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqException.protocol, 4);
      case 2:
        return LiftRetVal(MoqException.media, 4);
      case 3:
        return LiftRetVal(MoqException.mux, 4);
      case 4:
        return LiftRetVal(MoqException.jsonTrack, 4);
      case 5:
        return LiftRetVal(MoqException.url, 4);
      case 6:
        return LiftRetVal(MoqException.timeOverflow, 4);
      case 7:
        return LiftRetVal(MoqException.logLevel, 4);
      case 8:
        return LiftRetVal(MoqException.task, 4);
      case 9:
        return LiftRetVal(MoqException.json, 4);
      case 10:
        return LiftRetVal(MoqException.cancelled, 4);
      case 11:
        return LiftRetVal(MoqException.closed, 4);
      case 12:
        return LiftRetVal(MoqException.connect, 4);
      case 13:
        return LiftRetVal(MoqException.bind, 4);
      case 14:
        return LiftRetVal(MoqException.reject, 4);
      case 15:
        return LiftRetVal(MoqException.alreadyResponded, 4);
      case 16:
        return LiftRetVal(MoqException.codec, 4);
      case 17:
        return LiftRetVal(MoqException.unauthorized, 4);
      case 18:
        return LiftRetVal(MoqException.forbidden, 4);
      case 19:
        return LiftRetVal(MoqException.notFound, 4);
      case 20:
        return LiftRetVal(MoqException.unsupported, 4);
      case 21:
        return LiftRetVal(MoqException.alreadyCommitted, 4);
      case 22:
        return LiftRetVal(MoqException.invalidRoute, 4);
      case 23:
        return LiftRetVal(MoqException.unresolvableBroadcast, 4);
      case 24:
        return LiftRetVal(MoqException.log, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqException lift(RustBuffer buffer) {
    return FfiConverterMoqException.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqException input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqException _value) {
    return 4;
  }

  static int write(MoqException value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

class MoqExceptionErrorHandler extends UniffiRustCallStatusErrorHandler {
  @override
  Exception lift(RustBuffer errorBuf) {
    return FfiConverterMoqException.lift(errorBuf);
  }
}

final MoqExceptionErrorHandler moqExceptionErrorHandler =
    MoqExceptionErrorHandler();

enum MoqAudioFormat { aac, opus, flac, mp3 }

class FfiConverterMoqAudioFormat {
  static LiftRetVal<MoqAudioFormat> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqAudioFormat.aac, 4);
      case 2:
        return LiftRetVal(MoqAudioFormat.opus, 4);
      case 3:
        return LiftRetVal(MoqAudioFormat.flac, 4);
      case 4:
        return LiftRetVal(MoqAudioFormat.mp3, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqAudioFormat lift(RustBuffer buffer) {
    return FfiConverterMoqAudioFormat.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqAudioFormat input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqAudioFormat _value) {
    return 4;
  }

  static int write(MoqAudioFormat value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

abstract class MoqContainer {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqContainer {
  static MoqContainer lift(RustBuffer buffer) {
    return FfiConverterMoqContainer.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqContainer> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = LegacyMoqContainer.read(subview);
        return LiftRetVal<MoqContainer>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = CmafMoqContainer.read(subview);
        return LiftRetVal<MoqContainer>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 3:
        final lifted = LocMoqContainer.read(subview);
        return LiftRetVal<MoqContainer>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static RustBuffer lower(MoqContainer value) {
    return value.lower();
  }

  static int allocationSize(MoqContainer value) {
    return value.allocationSize();
  }

  static int write(MoqContainer value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class LegacyMoqContainer extends MoqContainer {
  LegacyMoqContainer();
  LegacyMoqContainer._();
  static LiftRetVal<LegacyMoqContainer> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(LegacyMoqContainer._(), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class CmafMoqContainer extends MoqContainer {
  final Uint8List init;
  CmafMoqContainer(Uint8List this.init);
  CmafMoqContainer._(Uint8List this.init);
  static LiftRetVal<CmafMoqContainer> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final init_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final init = init_lifted.value;
    new_offset += init_lifted.bytesRead;
    return LiftRetVal(CmafMoqContainer._(init), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUint8List.allocationSize(init) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUint8List.write(
      init,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class LocMoqContainer extends MoqContainer {
  LocMoqContainer();
  LocMoqContainer._();
  static LiftRetVal<LocMoqContainer> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(LocMoqContainer._(), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 3);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

enum MoqContainerFormat { fmp4, mkv, ts, flv }

class FfiConverterMoqContainerFormat {
  static LiftRetVal<MoqContainerFormat> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqContainerFormat.fmp4, 4);
      case 2:
        return LiftRetVal(MoqContainerFormat.mkv, 4);
      case 3:
        return LiftRetVal(MoqContainerFormat.ts, 4);
      case 4:
        return LiftRetVal(MoqContainerFormat.flv, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqContainerFormat lift(RustBuffer buffer) {
    return FfiConverterMoqContainerFormat.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqContainerFormat input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqContainerFormat _value) {
    return 4;
  }

  static int write(MoqContainerFormat value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

enum MoqVideoFormat { avc1, avc3, hvc1, hev1, av01, vp8, vp9 }

class FfiConverterMoqVideoFormat {
  static LiftRetVal<MoqVideoFormat> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqVideoFormat.avc1, 4);
      case 2:
        return LiftRetVal(MoqVideoFormat.avc3, 4);
      case 3:
        return LiftRetVal(MoqVideoFormat.hvc1, 4);
      case 4:
        return LiftRetVal(MoqVideoFormat.hev1, 4);
      case 5:
        return LiftRetVal(MoqVideoFormat.av01, 4);
      case 6:
        return LiftRetVal(MoqVideoFormat.vp8, 4);
      case 7:
        return LiftRetVal(MoqVideoFormat.vp9, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqVideoFormat lift(RustBuffer buffer) {
    return FfiConverterMoqVideoFormat.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqVideoFormat input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqVideoFormat _value) {
    return 4;
  }

  static int write(MoqVideoFormat value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

enum MoqConnectionStatus { connected, disconnected, migrating }

class FfiConverterMoqConnectionStatus {
  static LiftRetVal<MoqConnectionStatus> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqConnectionStatus.connected, 4);
      case 2:
        return LiftRetVal(MoqConnectionStatus.disconnected, 4);
      case 3:
        return LiftRetVal(MoqConnectionStatus.migrating, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqConnectionStatus lift(RustBuffer buffer) {
    return FfiConverterMoqConnectionStatus.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqConnectionStatus input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqConnectionStatus _value) {
    return 4;
  }

  static int write(MoqConnectionStatus value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

abstract class MoqBroadcastConsumerInterface {
  Future<MoqGroupConsumer> fetchGroup({
    required String name,
    required int sequence,
    required MoqFetchGroupOptions? options,
  });
  Future<MoqMediaGroupConsumer> fetchMediaGroup({
    required String name,
    required int sequence,
    required MoqContainer container,
    required MoqFetchGroupOptions? options,
  });
  Future<MoqBroadcastConsumer> resolve({required String? reference});
  Future<MoqCatalogConsumer> subscribeCatalog();
  Future<MoqMediaConsumer> subscribeMedia({
    required String name,
    required MoqContainer container,
    required MoqSubscription? subscription,
  });
  Future<MoqTrackConsumer> subscribeTrack({
    required String name,
    required MoqSubscription? subscription,
  });
  Future<MoqJsonSnapshotConsumer> subscribeJsonSnapshot({
    required String name,
    required MoqJsonSnapshotConfig config,
  });
  Future<MoqJsonStreamConsumer> subscribeJsonStream({
    required String name,
    required MoqJsonStreamConfig config,
  });
}

final _MoqBroadcastConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqbroadcastconsumer(ptr, status),
  );
});

class MoqBroadcastConsumer implements MoqBroadcastConsumerInterface {
  late final Pointer<Void> _ptr;
  MoqBroadcastConsumer._(this._ptr) {
    _MoqBroadcastConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqBroadcastConsumer.lift(Pointer<Void> ptr) {
    return MoqBroadcastConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbroadcastconsumer(_ptr, status),
    );
  }

  void dispose() {
    _MoqBroadcastConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbroadcastconsumer(_ptr, status),
    );
  }

  Future<MoqGroupConsumer> fetchGroup({
    required String name,
    required int sequence,
    required MoqFetchGroupOptions? options,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastconsumer_fetch_group(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        FfiConverterUInt64.lower(sequence),
        FfiConverterOptionalMoqFetchGroupOptions.lower(options),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqGroupConsumer.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  Future<MoqMediaGroupConsumer> fetchMediaGroup({
    required String name,
    required int sequence,
    required MoqContainer container,
    required MoqFetchGroupOptions? options,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastconsumer_fetch_media_group(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        FfiConverterUInt64.lower(sequence),
        FfiConverterMoqContainer.lower(container),
        FfiConverterOptionalMoqFetchGroupOptions.lower(options),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqMediaGroupConsumer.lift(
        Pointer<Void>.fromAddress(ptr),
      ),
      moqExceptionErrorHandler,
    );
  }

  Future<MoqBroadcastConsumer> resolve({required String? reference}) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastconsumer_resolve(
        uniffiClonePointer(),
        FfiConverterOptionalString.lower(reference),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqBroadcastConsumer.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  Future<MoqCatalogConsumer> subscribeCatalog() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_catalog(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqCatalogConsumer.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  Future<MoqMediaConsumer> subscribeMedia({
    required String name,
    required MoqContainer container,
    required MoqSubscription? subscription,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_media(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        FfiConverterMoqContainer.lower(container),
        FfiConverterOptionalMoqSubscription.lower(subscription),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqMediaConsumer.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  Future<MoqTrackConsumer> subscribeTrack({
    required String name,
    required MoqSubscription? subscription,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_track(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        FfiConverterOptionalMoqSubscription.lower(subscription),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqTrackConsumer.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  Future<MoqJsonSnapshotConsumer> subscribeJsonSnapshot({
    required String name,
    required MoqJsonSnapshotConfig config,
  }) {
    return uniffiRustCallAsync(
      () =>
          uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_json_snapshot(
            uniffiClonePointer(),
            FfiConverterString.lower(name),
            FfiConverterMoqJsonSnapshotConfig.lower(config),
          ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqJsonSnapshotConsumer.lift(
        Pointer<Void>.fromAddress(ptr),
      ),
      moqExceptionErrorHandler,
    );
  }

  Future<MoqJsonStreamConsumer> subscribeJsonStream({
    required String name,
    required MoqJsonStreamConfig config,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_json_stream(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        FfiConverterMoqJsonStreamConfig.lower(config),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqJsonStreamConsumer.lift(
        Pointer<Void>.fromAddress(ptr),
      ),
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqBroadcastConsumer {
  static MoqBroadcastConsumer lift(Pointer<Void> ptr) {
    return MoqBroadcastConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqBroadcastConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqBroadcastConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqBroadcastConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqBroadcastConsumer.lift(pointer), 8);
  }

  static int write(MoqBroadcastConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqCatalogConsumerInterface {
  void cancel();
  Future<MoqCatalog?> next();
}

final _MoqCatalogConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqcatalogconsumer(ptr, status));
});

class MoqCatalogConsumer implements MoqCatalogConsumerInterface {
  late final Pointer<Void> _ptr;
  MoqCatalogConsumer._(this._ptr) {
    _MoqCatalogConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqCatalogConsumer.lift(Pointer<Void> ptr) {
    return MoqCatalogConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqcatalogconsumer(_ptr, status),
    );
  }

  void dispose() {
    _MoqCatalogConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqcatalogconsumer(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqcatalogconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<MoqCatalog?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqcatalogconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqCatalog.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqCatalogConsumer {
  static MoqCatalogConsumer lift(Pointer<Void> ptr) {
    return MoqCatalogConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqCatalogConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqCatalogConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqCatalogConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqCatalogConsumer.lift(pointer), 8);
  }

  static int write(MoqCatalogConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqGroupConsumerInterface {
  void cancel();
  Future<MoqFrame?> readFrame();
  int sequence();
}

final _MoqGroupConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqgroupconsumer(ptr, status));
});

class MoqGroupConsumer implements MoqGroupConsumerInterface {
  late final Pointer<Void> _ptr;
  MoqGroupConsumer._(this._ptr) {
    _MoqGroupConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqGroupConsumer.lift(Pointer<Void> ptr) {
    return MoqGroupConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqgroupconsumer(_ptr, status),
    );
  }

  void dispose() {
    _MoqGroupConsumerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqgroupconsumer(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqgroupconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<MoqFrame?> readFrame() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqgroupconsumer_read_frame(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqFrame.lift,
      moqExceptionErrorHandler,
    );
  }

  int sequence() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqgroupconsumer_sequence(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      null,
    );
  }
}

class FfiConverterMoqGroupConsumer {
  static MoqGroupConsumer lift(Pointer<Void> ptr) {
    return MoqGroupConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqGroupConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqGroupConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqGroupConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqGroupConsumer.lift(pointer), 8);
  }

  static int write(MoqGroupConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaConsumerInterface {
  void cancel();
  Future<MoqMediaFrame?> next();
}

final _MoqMediaConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqmediaconsumer(ptr, status));
});

class MoqMediaConsumer implements MoqMediaConsumerInterface {
  late final Pointer<Void> _ptr;
  MoqMediaConsumer._(this._ptr) {
    _MoqMediaConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqMediaConsumer.lift(Pointer<Void> ptr) {
    return MoqMediaConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediaconsumer(_ptr, status),
    );
  }

  void dispose() {
    _MoqMediaConsumerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqmediaconsumer(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediaconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<MoqMediaFrame?> next() {
    return uniffiRustCallAsync(
      () =>
          uniffi_moq_ffi_fn_method_moqmediaconsumer_next(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqMediaFrame.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqMediaConsumer {
  static MoqMediaConsumer lift(Pointer<Void> ptr) {
    return MoqMediaConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaConsumer.lift(pointer), 8);
  }

  static int write(MoqMediaConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaGroupConsumerInterface {
  void cancel();
  Future<MoqMediaFrame?> next();
  int sequence();
}

final _MoqMediaGroupConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqmediagroupconsumer(ptr, status),
  );
});

class MoqMediaGroupConsumer implements MoqMediaGroupConsumerInterface {
  late final Pointer<Void> _ptr;
  MoqMediaGroupConsumer._(this._ptr) {
    _MoqMediaGroupConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqMediaGroupConsumer.lift(Pointer<Void> ptr) {
    return MoqMediaGroupConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediagroupconsumer(_ptr, status),
    );
  }

  void dispose() {
    _MoqMediaGroupConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqmediagroupconsumer(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediagroupconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<MoqMediaFrame?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqmediagroupconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqMediaFrame.lift,
      moqExceptionErrorHandler,
    );
  }

  int sequence() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqmediagroupconsumer_sequence(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      null,
    );
  }
}

class FfiConverterMoqMediaGroupConsumer {
  static MoqMediaGroupConsumer lift(Pointer<Void> ptr) {
    return MoqMediaGroupConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaGroupConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaGroupConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaGroupConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaGroupConsumer.lift(pointer), 8);
  }

  static int write(MoqMediaGroupConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqTrackConsumerInterface {
  void cancel();
  MoqTrackInfo info();
  Future<MoqGroupConsumer?> nextGroup();
  Future<MoqFrame?> readFrame();
  Future<MoqDatagram?> recvDatagram();
  Future<MoqGroupConsumer?> recvGroup();
  void update({required MoqSubscription subscription});
}

final _MoqTrackConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackconsumer(ptr, status));
});

class MoqTrackConsumer implements MoqTrackConsumerInterface {
  late final Pointer<Void> _ptr;
  MoqTrackConsumer._(this._ptr) {
    _MoqTrackConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTrackConsumer.lift(Pointer<Void> ptr) {
    return MoqTrackConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtrackconsumer(_ptr, status),
    );
  }

  void dispose() {
    _MoqTrackConsumerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackconsumer(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtrackconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  MoqTrackInfo info() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackconsumer_info(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackInfo.lift,
      moqExceptionErrorHandler,
    );
  }

  Future<MoqGroupConsumer?> nextGroup() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackconsumer_next_group(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqGroupConsumer.lift,
      moqExceptionErrorHandler,
    );
  }

  Future<MoqFrame?> readFrame() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackconsumer_read_frame(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqFrame.lift,
      moqExceptionErrorHandler,
    );
  }

  Future<MoqDatagram?> recvDatagram() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackconsumer_recv_datagram(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqDatagram.lift,
      moqExceptionErrorHandler,
    );
  }

  Future<MoqGroupConsumer?> recvGroup() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackconsumer_recv_group(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqGroupConsumer.lift,
      moqExceptionErrorHandler,
    );
  }

  void update({required MoqSubscription subscription}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtrackconsumer_update(
        uniffiClonePointer(),
        FfiConverterMoqSubscription.lower(subscription),
        status,
      );
    }, null);
  }
}

class FfiConverterMoqTrackConsumer {
  static MoqTrackConsumer lift(Pointer<Void> ptr) {
    return MoqTrackConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqTrackConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqTrackConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqTrackConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqTrackConsumer.lift(pointer), 8);
  }

  static int write(MoqTrackConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqJsonSnapshotConsumerInterface {
  void cancel();
  Future<String?> next();
}

final _MoqJsonSnapshotConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonsnapshotconsumer(ptr, status),
  );
});

class MoqJsonSnapshotConsumer implements MoqJsonSnapshotConsumerInterface {
  late final Pointer<Void> _ptr;
  MoqJsonSnapshotConsumer._(this._ptr) {
    _MoqJsonSnapshotConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonSnapshotConsumer.lift(Pointer<Void> ptr) {
    return MoqJsonSnapshotConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonsnapshotconsumer(_ptr, status),
    );
  }

  void dispose() {
    _MoqJsonSnapshotConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqjsonsnapshotconsumer(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonsnapshotconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<String?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqjsonsnapshotconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalString.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqJsonSnapshotConsumer {
  static MoqJsonSnapshotConsumer lift(Pointer<Void> ptr) {
    return MoqJsonSnapshotConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqJsonSnapshotConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqJsonSnapshotConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqJsonSnapshotConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqJsonSnapshotConsumer.lift(pointer), 8);
  }

  static int write(MoqJsonSnapshotConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqJsonSnapshotProducerInterface {
  void finish();
  void update({required String value});
}

final _MoqJsonSnapshotProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonsnapshotproducer(ptr, status),
  );
});

class MoqJsonSnapshotProducer implements MoqJsonSnapshotProducerInterface {
  late final Pointer<Void> _ptr;
  MoqJsonSnapshotProducer._(this._ptr) {
    _MoqJsonSnapshotProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonSnapshotProducer.lift(Pointer<Void> ptr) {
    return MoqJsonSnapshotProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonsnapshotproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqJsonSnapshotProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqjsonsnapshotproducer(_ptr, status),
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonsnapshotproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void update({required String value}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonsnapshotproducer_update(
        uniffiClonePointer(),
        FfiConverterString.lower(value),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqJsonSnapshotProducer {
  static MoqJsonSnapshotProducer lift(Pointer<Void> ptr) {
    return MoqJsonSnapshotProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqJsonSnapshotProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqJsonSnapshotProducer value) {
    return 8;
  }

  static LiftRetVal<MoqJsonSnapshotProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqJsonSnapshotProducer.lift(pointer), 8);
  }

  static int write(MoqJsonSnapshotProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqJsonStreamConsumerInterface {
  void cancel();
  Future<String?> next();
}

final _MoqJsonStreamConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonstreamconsumer(ptr, status),
  );
});

class MoqJsonStreamConsumer implements MoqJsonStreamConsumerInterface {
  late final Pointer<Void> _ptr;
  MoqJsonStreamConsumer._(this._ptr) {
    _MoqJsonStreamConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonStreamConsumer.lift(Pointer<Void> ptr) {
    return MoqJsonStreamConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonstreamconsumer(_ptr, status),
    );
  }

  void dispose() {
    _MoqJsonStreamConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqjsonstreamconsumer(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonstreamconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<String?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqjsonstreamconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalString.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqJsonStreamConsumer {
  static MoqJsonStreamConsumer lift(Pointer<Void> ptr) {
    return MoqJsonStreamConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqJsonStreamConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqJsonStreamConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqJsonStreamConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqJsonStreamConsumer.lift(pointer), 8);
  }

  static int write(MoqJsonStreamConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqJsonStreamProducerInterface {
  void append({required String value});
  void finish();
}

final _MoqJsonStreamProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonstreamproducer(ptr, status),
  );
});

class MoqJsonStreamProducer implements MoqJsonStreamProducerInterface {
  late final Pointer<Void> _ptr;
  MoqJsonStreamProducer._(this._ptr) {
    _MoqJsonStreamProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonStreamProducer.lift(Pointer<Void> ptr) {
    return MoqJsonStreamProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonstreamproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqJsonStreamProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqjsonstreamproducer(_ptr, status),
    );
  }

  void append({required String value}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonstreamproducer_append(
        uniffiClonePointer(),
        FfiConverterString.lower(value),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonstreamproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqJsonStreamProducer {
  static MoqJsonStreamProducer lift(Pointer<Void> ptr) {
    return MoqJsonStreamProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqJsonStreamProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqJsonStreamProducer value) {
    return 8;
  }

  static LiftRetVal<MoqJsonStreamProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqJsonStreamProducer.lift(pointer), 8);
  }

  static int write(MoqJsonStreamProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAnnounceInterface {
  void cancel();
  void update({required MoqRoute route});
}

final _MoqAnnounceFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqannounce(ptr, status));
});

class MoqAnnounce implements MoqAnnounceInterface {
  late final Pointer<Void> _ptr;
  MoqAnnounce._(this._ptr) {
    _MoqAnnounceFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAnnounce.lift(Pointer<Void> ptr) {
    return MoqAnnounce._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqannounce(_ptr, status),
    );
  }

  void dispose() {
    _MoqAnnounceFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqannounce(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqannounce_cancel(uniffiClonePointer(), status);
    }, null);
  }

  void update({required MoqRoute route}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqannounce_update(
        uniffiClonePointer(),
        FfiConverterMoqRoute.lower(route),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqAnnounce {
  static MoqAnnounce lift(Pointer<Void> ptr) {
    return MoqAnnounce.lift(ptr);
  }

  static Pointer<Void> lower(MoqAnnounce value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAnnounce value) {
    return 8;
  }

  static LiftRetVal<MoqAnnounce> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAnnounce.lift(pointer), 8);
  }

  static int write(MoqAnnounce value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAnnouncedInterface {
  void cancel();
  Future<MoqAnnouncement?> next();
}

final _MoqAnnouncedFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqannounced(ptr, status));
});

class MoqAnnounced implements MoqAnnouncedInterface {
  late final Pointer<Void> _ptr;
  MoqAnnounced._(this._ptr) {
    _MoqAnnouncedFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAnnounced.lift(Pointer<Void> ptr) {
    return MoqAnnounced._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqannounced(_ptr, status),
    );
  }

  void dispose() {
    _MoqAnnouncedFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqannounced(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqannounced_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<MoqAnnouncement?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqannounced_next(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqAnnouncement.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqAnnounced {
  static MoqAnnounced lift(Pointer<Void> ptr) {
    return MoqAnnounced.lift(ptr);
  }

  static Pointer<Void> lower(MoqAnnounced value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAnnounced value) {
    return 8;
  }

  static LiftRetVal<MoqAnnounced> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAnnounced.lift(pointer), 8);
  }

  static int write(MoqAnnounced value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAnnouncedBroadcastInterface {
  Future<MoqBroadcastConsumer> available();
  void cancel();
}

final _MoqAnnouncedBroadcastFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqannouncedbroadcast(ptr, status),
  );
});

class MoqAnnouncedBroadcast implements MoqAnnouncedBroadcastInterface {
  late final Pointer<Void> _ptr;
  MoqAnnouncedBroadcast._(this._ptr) {
    _MoqAnnouncedBroadcastFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAnnouncedBroadcast.lift(Pointer<Void> ptr) {
    return MoqAnnouncedBroadcast._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqannouncedbroadcast(_ptr, status),
    );
  }

  void dispose() {
    _MoqAnnouncedBroadcastFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqannouncedbroadcast(_ptr, status),
    );
  }

  Future<MoqBroadcastConsumer> available() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqannouncedbroadcast_available(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqBroadcastConsumer.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqannouncedbroadcast_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }
}

class FfiConverterMoqAnnouncedBroadcast {
  static MoqAnnouncedBroadcast lift(Pointer<Void> ptr) {
    return MoqAnnouncedBroadcast.lift(ptr);
  }

  static Pointer<Void> lower(MoqAnnouncedBroadcast value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAnnouncedBroadcast value) {
    return 8;
  }

  static LiftRetVal<MoqAnnouncedBroadcast> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAnnouncedBroadcast.lift(pointer), 8);
  }

  static int write(MoqAnnouncedBroadcast value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAnnouncementInterface {
  bool active();
  String path();
  MoqRoute route();
}

final _MoqAnnouncementFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqannouncement(ptr, status));
});

class MoqAnnouncement implements MoqAnnouncementInterface {
  late final Pointer<Void> _ptr;
  MoqAnnouncement._(this._ptr) {
    _MoqAnnouncementFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAnnouncement.lift(Pointer<Void> ptr) {
    return MoqAnnouncement._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqannouncement(_ptr, status),
    );
  }

  void dispose() {
    _MoqAnnouncementFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqannouncement(_ptr, status));
  }

  bool active() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqannouncement_active(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterBool.lift,
      null,
    );
  }

  String path() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqannouncement_path(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterString.lift,
      null,
    );
  }

  MoqRoute route() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqannouncement_route(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqRoute.lift,
      null,
    );
  }
}

class FfiConverterMoqAnnouncement {
  static MoqAnnouncement lift(Pointer<Void> ptr) {
    return MoqAnnouncement.lift(ptr);
  }

  static Pointer<Void> lower(MoqAnnouncement value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAnnouncement value) {
    return 8;
  }

  static LiftRetVal<MoqAnnouncement> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAnnouncement.lift(pointer), 8);
  }

  static int write(MoqAnnouncement value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqBroadcastRequestInterface {
  void abort({required int errorCode});
  void accept({required MoqBroadcastProducer broadcast});
  String path();
}

final _MoqBroadcastRequestFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqbroadcastrequest(ptr, status));
});

class MoqBroadcastRequest implements MoqBroadcastRequestInterface {
  late final Pointer<Void> _ptr;
  MoqBroadcastRequest._(this._ptr) {
    _MoqBroadcastRequestFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqBroadcastRequest.lift(Pointer<Void> ptr) {
    return MoqBroadcastRequest._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbroadcastrequest(_ptr, status),
    );
  }

  void dispose() {
    _MoqBroadcastRequestFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbroadcastrequest(_ptr, status),
    );
  }

  void abort({required int errorCode}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastrequest_abort(
        uniffiClonePointer(),
        FfiConverterUInt16.lower(errorCode),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void accept({required MoqBroadcastProducer broadcast}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastrequest_accept(
        uniffiClonePointer(),
        FfiConverterMoqBroadcastProducer.lower(broadcast),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  String path() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastrequest_path(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterString.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqBroadcastRequest {
  static MoqBroadcastRequest lift(Pointer<Void> ptr) {
    return MoqBroadcastRequest.lift(ptr);
  }

  static Pointer<Void> lower(MoqBroadcastRequest value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqBroadcastRequest value) {
    return 8;
  }

  static LiftRetVal<MoqBroadcastRequest> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqBroadcastRequest.lift(pointer), 8);
  }

  static int write(MoqBroadcastRequest value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqOriginConsumerInterface {
  MoqAnnounced announced({required String prefix});
  MoqAnnouncedBroadcast announcedBroadcast({required String path});
  Future<MoqBroadcastConsumer> requestBroadcast({required String path});
}

final _MoqOriginConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqoriginconsumer(ptr, status));
});

class MoqOriginConsumer implements MoqOriginConsumerInterface {
  late final Pointer<Void> _ptr;
  MoqOriginConsumer._(this._ptr) {
    _MoqOriginConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqOriginConsumer.lift(Pointer<Void> ptr) {
    return MoqOriginConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqoriginconsumer(_ptr, status),
    );
  }

  void dispose() {
    _MoqOriginConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqoriginconsumer(_ptr, status),
    );
  }

  MoqAnnounced announced({required String prefix}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqoriginconsumer_announced(
        uniffiClonePointer(),
        FfiConverterString.lower(prefix),
        status,
      ),
      FfiConverterMoqAnnounced.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqAnnouncedBroadcast announcedBroadcast({required String path}) {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqoriginconsumer_announced_broadcast(
            uniffiClonePointer(),
            FfiConverterString.lower(path),
            status,
          ),
      FfiConverterMoqAnnouncedBroadcast.lift,
      moqExceptionErrorHandler,
    );
  }

  Future<MoqBroadcastConsumer> requestBroadcast({required String path}) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqoriginconsumer_request_broadcast(
        uniffiClonePointer(),
        FfiConverterString.lower(path),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqBroadcastConsumer.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqOriginConsumer {
  static MoqOriginConsumer lift(Pointer<Void> ptr) {
    return MoqOriginConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqOriginConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqOriginConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqOriginConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqOriginConsumer.lift(pointer), 8);
  }

  static int write(MoqOriginConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqOriginDynamicInterface {
  void cancel();
  Future<MoqBroadcastRequest> requestedBroadcast();
}

final _MoqOriginDynamicFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqorigindynamic(ptr, status));
});

class MoqOriginDynamic implements MoqOriginDynamicInterface {
  late final Pointer<Void> _ptr;
  MoqOriginDynamic._(this._ptr) {
    _MoqOriginDynamicFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqOriginDynamic.lift(Pointer<Void> ptr) {
    return MoqOriginDynamic._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqorigindynamic(_ptr, status),
    );
  }

  void dispose() {
    _MoqOriginDynamicFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqorigindynamic(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqorigindynamic_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<MoqBroadcastRequest> requestedBroadcast() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqorigindynamic_requested_broadcast(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqBroadcastRequest.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqOriginDynamic {
  static MoqOriginDynamic lift(Pointer<Void> ptr) {
    return MoqOriginDynamic.lift(ptr);
  }

  static Pointer<Void> lower(MoqOriginDynamic value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqOriginDynamic value) {
    return 8;
  }

  static LiftRetVal<MoqOriginDynamic> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqOriginDynamic.lift(pointer), 8);
  }

  static int write(MoqOriginDynamic value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqOriginProducerInterface {
  MoqAnnounce announce({required String prefix, required MoqRoute route});
  MoqOriginConsumer consume();
  MoqBroadcastProducer createBroadcast({required String path});
  MoqOriginDynamic dynamic_();
}

final _MoqOriginProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqoriginproducer(ptr, status));
});

class MoqOriginProducer implements MoqOriginProducerInterface {
  late final Pointer<Void> _ptr;
  MoqOriginProducer._(this._ptr) {
    _MoqOriginProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqOriginProducer({required MoqOriginOptions options})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqoriginproducer_new(
          FfiConverterMoqOriginOptions.lower(options),
          status,
        ),
        null,
      ) {
    _MoqOriginProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqOriginProducer.lift(Pointer<Void> ptr) {
    return MoqOriginProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqoriginproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqOriginProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqoriginproducer(_ptr, status),
    );
  }

  MoqAnnounce announce({required String prefix, required MoqRoute route}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqoriginproducer_announce(
        uniffiClonePointer(),
        FfiConverterString.lower(prefix),
        FfiConverterMoqRoute.lower(route),
        status,
      ),
      FfiConverterMoqAnnounce.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqOriginConsumer consume() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqoriginproducer_consume(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqOriginConsumer.lift,
      null,
    );
  }

  MoqBroadcastProducer createBroadcast({required String path}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqoriginproducer_create_broadcast(
        uniffiClonePointer(),
        FfiConverterString.lower(path),
        status,
      ),
      FfiConverterMoqBroadcastProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqOriginDynamic dynamic_() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqoriginproducer_dynamic(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqOriginDynamic.lift,
      null,
    );
  }
}

class FfiConverterMoqOriginProducer {
  static MoqOriginProducer lift(Pointer<Void> ptr) {
    return MoqOriginProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqOriginProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqOriginProducer value) {
    return 8;
  }

  static LiftRetVal<MoqOriginProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqOriginProducer.lift(pointer), 8);
  }

  static int write(MoqOriginProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqBroadcastDynamicInterface {
  void cancel();
  Future<MoqTrackRequest> requestedTrack();
}

final _MoqBroadcastDynamicFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqbroadcastdynamic(ptr, status));
});

class MoqBroadcastDynamic implements MoqBroadcastDynamicInterface {
  late final Pointer<Void> _ptr;
  MoqBroadcastDynamic._(this._ptr) {
    _MoqBroadcastDynamicFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqBroadcastDynamic.lift(Pointer<Void> ptr) {
    return MoqBroadcastDynamic._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbroadcastdynamic(_ptr, status),
    );
  }

  void dispose() {
    _MoqBroadcastDynamicFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbroadcastdynamic(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastdynamic_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<MoqTrackRequest> requestedTrack() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastdynamic_requested_track(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqTrackRequest.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqBroadcastDynamic {
  static MoqBroadcastDynamic lift(Pointer<Void> ptr) {
    return MoqBroadcastDynamic.lift(ptr);
  }

  static Pointer<Void> lower(MoqBroadcastDynamic value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqBroadcastDynamic value) {
    return 8;
  }

  static LiftRetVal<MoqBroadcastDynamic> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqBroadcastDynamic.lift(pointer), 8);
  }

  static int write(MoqBroadcastDynamic value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqBroadcastProducerInterface {
  MoqJsonSnapshotProducer publishJsonSnapshot({
    required String name,
    required MoqJsonSnapshotConfig config,
  });
  MoqJsonStreamProducer publishJsonStream({
    required String name,
    required MoqJsonStreamConfig config,
  });
  MoqBroadcastConsumer consume();
  MoqBroadcastDynamic dynamic_();
  void finish();
  MoqMediaProducer publishAudio({required MoqAudioInit init});
  MoqMediaProducer publishAudioOnTrack({
    required MoqTrackRequest request,
    required MoqAudioInit init,
  });
  MoqContainerProducer publishContainer({required MoqContainerInit init});
  MoqContainerStreamProducer publishContainerStream({
    required MoqContainerFormat format,
  });
  MoqTrackProducer publishTrack({
    required String name,
    required MoqTrackInfo? info,
  });
  MoqMediaProducer publishVideo({required MoqVideoInit init});
  MoqMediaProducer publishVideoOnTrack({
    required MoqTrackRequest request,
    required MoqVideoInit init,
  });
  MoqMediaStreamProducer publishVideoStream({required MoqVideoInit init});
  void removeCatalogSection({required String name});
  void setAnnounce({required bool announce});
  void setCatalogSection({required String name, required String json});
  void setVideoProperties({required MoqVideoProperties properties});
}

final _MoqBroadcastProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqbroadcastproducer(ptr, status),
  );
});

class MoqBroadcastProducer implements MoqBroadcastProducerInterface {
  late final Pointer<Void> _ptr;
  MoqBroadcastProducer._(this._ptr) {
    _MoqBroadcastProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqBroadcastProducer()
    : _ptr = rustCall(
        (status) =>
            uniffi_moq_ffi_fn_constructor_moqbroadcastproducer_new(status),
        moqExceptionErrorHandler,
      ) {
    _MoqBroadcastProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqBroadcastProducer.lift(Pointer<Void> ptr) {
    return MoqBroadcastProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbroadcastproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqBroadcastProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbroadcastproducer(_ptr, status),
    );
  }

  MoqJsonSnapshotProducer publishJsonSnapshot({
    required String name,
    required MoqJsonSnapshotConfig config,
  }) {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_json_snapshot(
            uniffiClonePointer(),
            FfiConverterString.lower(name),
            FfiConverterMoqJsonSnapshotConfig.lower(config),
            status,
          ),
      FfiConverterMoqJsonSnapshotProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqJsonStreamProducer publishJsonStream({
    required String name,
    required MoqJsonStreamConfig config,
  }) {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_json_stream(
            uniffiClonePointer(),
            FfiConverterString.lower(name),
            FfiConverterMoqJsonStreamConfig.lower(config),
            status,
          ),
      FfiConverterMoqJsonStreamProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqBroadcastConsumer consume() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastproducer_consume(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqBroadcastConsumer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqBroadcastDynamic dynamic_() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastproducer_dynamic(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqBroadcastDynamic.lift,
      moqExceptionErrorHandler,
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqMediaProducer publishAudio({required MoqAudioInit init}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_audio(
        uniffiClonePointer(),
        FfiConverterMoqAudioInit.lower(init),
        status,
      ),
      FfiConverterMoqMediaProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqMediaProducer publishAudioOnTrack({
    required MoqTrackRequest request,
    required MoqAudioInit init,
  }) {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_audio_on_track(
            uniffiClonePointer(),
            FfiConverterMoqTrackRequest.lower(request),
            FfiConverterMoqAudioInit.lower(init),
            status,
          ),
      FfiConverterMoqMediaProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqContainerProducer publishContainer({required MoqContainerInit init}) {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_container(
            uniffiClonePointer(),
            FfiConverterMoqContainerInit.lower(init),
            status,
          ),
      FfiConverterMoqContainerProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqContainerStreamProducer publishContainerStream({
    required MoqContainerFormat format,
  }) {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_container_stream(
            uniffiClonePointer(),
            FfiConverterMoqContainerFormat.lower(format),
            status,
          ),
      FfiConverterMoqContainerStreamProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqTrackProducer publishTrack({
    required String name,
    required MoqTrackInfo? info,
  }) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_track(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        FfiConverterOptionalMoqTrackInfo.lower(info),
        status,
      ),
      FfiConverterMoqTrackProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqMediaProducer publishVideo({required MoqVideoInit init}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_video(
        uniffiClonePointer(),
        FfiConverterMoqVideoInit.lower(init),
        status,
      ),
      FfiConverterMoqMediaProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqMediaProducer publishVideoOnTrack({
    required MoqTrackRequest request,
    required MoqVideoInit init,
  }) {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_video_on_track(
            uniffiClonePointer(),
            FfiConverterMoqTrackRequest.lower(request),
            FfiConverterMoqVideoInit.lower(init),
            status,
          ),
      FfiConverterMoqMediaProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqMediaStreamProducer publishVideoStream({required MoqVideoInit init}) {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_video_stream(
            uniffiClonePointer(),
            FfiConverterMoqVideoInit.lower(init),
            status,
          ),
      FfiConverterMoqMediaStreamProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  void removeCatalogSection({required String name}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastproducer_remove_catalog_section(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void setAnnounce({required bool announce}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastproducer_set_announce(
        uniffiClonePointer(),
        FfiConverterBool.lower(announce),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void setCatalogSection({required String name, required String json}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastproducer_set_catalog_section(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        FfiConverterString.lower(json),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void setVideoProperties({required MoqVideoProperties properties}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastproducer_set_video_properties(
        uniffiClonePointer(),
        FfiConverterMoqVideoProperties.lower(properties),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqBroadcastProducer {
  static MoqBroadcastProducer lift(Pointer<Void> ptr) {
    return MoqBroadcastProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqBroadcastProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqBroadcastProducer value) {
    return 8;
  }

  static LiftRetVal<MoqBroadcastProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqBroadcastProducer.lift(pointer), 8);
  }

  static int write(MoqBroadcastProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqContainerProducerInterface {
  void cut();
  void finish();
  void seek({required int sequence});
  void write({required Uint8List payload});
}

final _MoqContainerProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqcontainerproducer(ptr, status),
  );
});

class MoqContainerProducer implements MoqContainerProducerInterface {
  late final Pointer<Void> _ptr;
  MoqContainerProducer._(this._ptr) {
    _MoqContainerProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqContainerProducer.lift(Pointer<Void> ptr) {
    return MoqContainerProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqcontainerproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqContainerProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqcontainerproducer(_ptr, status),
    );
  }

  void cut() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqcontainerproducer_cut(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqcontainerproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void seek({required int sequence}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqcontainerproducer_seek(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(sequence),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void write({required Uint8List payload}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqcontainerproducer_write(
        uniffiClonePointer(),
        FfiConverterUint8List.lower(payload),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqContainerProducer {
  static MoqContainerProducer lift(Pointer<Void> ptr) {
    return MoqContainerProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqContainerProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqContainerProducer value) {
    return 8;
  }

  static LiftRetVal<MoqContainerProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqContainerProducer.lift(pointer), 8);
  }

  static int write(MoqContainerProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqContainerStreamProducerInterface {
  void finish();
  void write({required Uint8List payload});
}

final _MoqContainerStreamProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqcontainerstreamproducer(ptr, status),
  );
});

class MoqContainerStreamProducer
    implements MoqContainerStreamProducerInterface {
  late final Pointer<Void> _ptr;
  MoqContainerStreamProducer._(this._ptr) {
    _MoqContainerStreamProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqContainerStreamProducer.lift(Pointer<Void> ptr) {
    return MoqContainerStreamProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) =>
          uniffi_moq_ffi_fn_clone_moqcontainerstreamproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqContainerStreamProducerFinalizer.detach(this);
    rustCall(
      (status) =>
          uniffi_moq_ffi_fn_free_moqcontainerstreamproducer(_ptr, status),
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqcontainerstreamproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void write({required Uint8List payload}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqcontainerstreamproducer_write(
        uniffiClonePointer(),
        FfiConverterUint8List.lower(payload),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqContainerStreamProducer {
  static MoqContainerStreamProducer lift(Pointer<Void> ptr) {
    return MoqContainerStreamProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqContainerStreamProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqContainerStreamProducer value) {
    return 8;
  }

  static LiftRetVal<MoqContainerStreamProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqContainerStreamProducer.lift(pointer), 8);
  }

  static int write(MoqContainerStreamProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqGroupProducerInterface {
  void abort({required int errorCode});
  MoqGroupConsumer consume();
  void finish();
  int sequence();
  void writeFrame({required MoqFrame frame});
}

final _MoqGroupProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqgroupproducer(ptr, status));
});

class MoqGroupProducer implements MoqGroupProducerInterface {
  late final Pointer<Void> _ptr;
  MoqGroupProducer._(this._ptr) {
    _MoqGroupProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqGroupProducer.lift(Pointer<Void> ptr) {
    return MoqGroupProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqgroupproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqGroupProducerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqgroupproducer(_ptr, status));
  }

  void abort({required int errorCode}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqgroupproducer_abort(
        uniffiClonePointer(),
        FfiConverterUInt16.lower(errorCode),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqGroupConsumer consume() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqgroupproducer_consume(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqGroupConsumer.lift,
      moqExceptionErrorHandler,
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqgroupproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  int sequence() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqgroupproducer_sequence(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      null,
    );
  }

  void writeFrame({required MoqFrame frame}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqgroupproducer_write_frame(
        uniffiClonePointer(),
        FfiConverterMoqFrame.lower(frame),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqGroupProducer {
  static MoqGroupProducer lift(Pointer<Void> ptr) {
    return MoqGroupProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqGroupProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqGroupProducer value) {
    return 8;
  }

  static LiftRetVal<MoqGroupProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqGroupProducer.lift(pointer), 8);
  }

  static int write(MoqGroupProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqGroupRequestInterface {
  void abort({required int errorCode});
  MoqGroupProducer accept();
  int priority();
  int sequence();
}

final _MoqGroupRequestFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqgrouprequest(ptr, status));
});

class MoqGroupRequest implements MoqGroupRequestInterface {
  late final Pointer<Void> _ptr;
  MoqGroupRequest._(this._ptr) {
    _MoqGroupRequestFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqGroupRequest.lift(Pointer<Void> ptr) {
    return MoqGroupRequest._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqgrouprequest(_ptr, status),
    );
  }

  void dispose() {
    _MoqGroupRequestFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqgrouprequest(_ptr, status));
  }

  void abort({required int errorCode}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqgrouprequest_abort(
        uniffiClonePointer(),
        FfiConverterUInt16.lower(errorCode),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqGroupProducer accept() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqgrouprequest_accept(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqGroupProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  int priority() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqgrouprequest_priority(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt8.lift,
      null,
    );
  }

  int sequence() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqgrouprequest_sequence(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      null,
    );
  }
}

class FfiConverterMoqGroupRequest {
  static MoqGroupRequest lift(Pointer<Void> ptr) {
    return MoqGroupRequest.lift(ptr);
  }

  static Pointer<Void> lower(MoqGroupRequest value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqGroupRequest value) {
    return 8;
  }

  static LiftRetVal<MoqGroupRequest> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqGroupRequest.lift(pointer), 8);
  }

  static int write(MoqGroupRequest value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaProducerInterface {
  void cut();
  void finish();
  String name();
  void seek({required int sequence});
  Future<void> unused();
  Future<void> used();
  void writeFrame({required MoqFrame frame});
}

final _MoqMediaProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqmediaproducer(ptr, status));
});

class MoqMediaProducer implements MoqMediaProducerInterface {
  late final Pointer<Void> _ptr;
  MoqMediaProducer._(this._ptr) {
    _MoqMediaProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqMediaProducer.lift(Pointer<Void> ptr) {
    return MoqMediaProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediaproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqMediaProducerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqmediaproducer(_ptr, status));
  }

  void cut() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediaproducer_cut(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediaproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  String name() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqmediaproducer_name(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterString.lift,
      moqExceptionErrorHandler,
    );
  }

  void seek({required int sequence}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediaproducer_seek(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(sequence),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  Future<void> unused() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqmediaproducer_unused(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  Future<void> used() {
    return uniffiRustCallAsync(
      () =>
          uniffi_moq_ffi_fn_method_moqmediaproducer_used(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  void writeFrame({required MoqFrame frame}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediaproducer_write_frame(
        uniffiClonePointer(),
        FfiConverterMoqFrame.lower(frame),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqMediaProducer {
  static MoqMediaProducer lift(Pointer<Void> ptr) {
    return MoqMediaProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaProducer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaProducer.lift(pointer), 8);
  }

  static int write(MoqMediaProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaStreamProducerInterface {
  void finish();
  void write({required Uint8List payload});
}

final _MoqMediaStreamProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqmediastreamproducer(ptr, status),
  );
});

class MoqMediaStreamProducer implements MoqMediaStreamProducerInterface {
  late final Pointer<Void> _ptr;
  MoqMediaStreamProducer._(this._ptr) {
    _MoqMediaStreamProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqMediaStreamProducer.lift(Pointer<Void> ptr) {
    return MoqMediaStreamProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediastreamproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqMediaStreamProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqmediastreamproducer(_ptr, status),
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediastreamproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void write({required Uint8List payload}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediastreamproducer_write(
        uniffiClonePointer(),
        FfiConverterUint8List.lower(payload),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqMediaStreamProducer {
  static MoqMediaStreamProducer lift(Pointer<Void> ptr) {
    return MoqMediaStreamProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaStreamProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaStreamProducer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaStreamProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaStreamProducer.lift(pointer), 8);
  }

  static int write(MoqMediaStreamProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqTrackDynamicInterface {
  void cancel();
  Future<MoqGroupRequest> requestedGroup();
}

final _MoqTrackDynamicFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackdynamic(ptr, status));
});

class MoqTrackDynamic implements MoqTrackDynamicInterface {
  late final Pointer<Void> _ptr;
  MoqTrackDynamic._(this._ptr) {
    _MoqTrackDynamicFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTrackDynamic.lift(Pointer<Void> ptr) {
    return MoqTrackDynamic._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtrackdynamic(_ptr, status),
    );
  }

  void dispose() {
    _MoqTrackDynamicFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackdynamic(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtrackdynamic_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Future<MoqGroupRequest> requestedGroup() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackdynamic_requested_group(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqGroupRequest.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqTrackDynamic {
  static MoqTrackDynamic lift(Pointer<Void> ptr) {
    return MoqTrackDynamic.lift(ptr);
  }

  static Pointer<Void> lower(MoqTrackDynamic value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqTrackDynamic value) {
    return 8;
  }

  static LiftRetVal<MoqTrackDynamic> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqTrackDynamic.lift(pointer), 8);
  }

  static int write(MoqTrackDynamic value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqTrackProducerInterface {
  void abort({required int errorCode});
  int appendDatagram({required MoqFrame frame});
  MoqGroupProducer appendGroup();
  MoqTrackConsumer consume({required MoqSubscription? subscription});
  MoqGroupProducer createGroup({required int sequence});
  MoqTrackDynamic dynamic_();
  void finish();
  void finishAt({required int finalSequence});
  String name();
  Future<void> unused();
  Future<void> used();
  void writeFrame({required MoqFrame frame});
}

final _MoqTrackProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackproducer(ptr, status));
});

class MoqTrackProducer implements MoqTrackProducerInterface {
  late final Pointer<Void> _ptr;
  MoqTrackProducer._(this._ptr) {
    _MoqTrackProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTrackProducer.lift(Pointer<Void> ptr) {
    return MoqTrackProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtrackproducer(_ptr, status),
    );
  }

  void dispose() {
    _MoqTrackProducerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackproducer(_ptr, status));
  }

  void abort({required int errorCode}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtrackproducer_abort(
        uniffiClonePointer(),
        FfiConverterUInt16.lower(errorCode),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  int appendDatagram({required MoqFrame frame}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackproducer_append_datagram(
        uniffiClonePointer(),
        FfiConverterMoqFrame.lower(frame),
        status,
      ),
      FfiConverterUInt64.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqGroupProducer appendGroup() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackproducer_append_group(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqGroupProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqTrackConsumer consume({required MoqSubscription? subscription}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackproducer_consume(
        uniffiClonePointer(),
        FfiConverterOptionalMoqSubscription.lower(subscription),
        status,
      ),
      FfiConverterMoqTrackConsumer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqGroupProducer createGroup({required int sequence}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackproducer_create_group(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(sequence),
        status,
      ),
      FfiConverterMoqGroupProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqTrackDynamic dynamic_() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackproducer_dynamic(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDynamic.lift,
      moqExceptionErrorHandler,
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtrackproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void finishAt({required int finalSequence}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtrackproducer_finish_at(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(finalSequence),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  String name() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackproducer_name(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterString.lift,
      moqExceptionErrorHandler,
    );
  }

  Future<void> unused() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackproducer_unused(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  Future<void> used() {
    return uniffiRustCallAsync(
      () =>
          uniffi_moq_ffi_fn_method_moqtrackproducer_used(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  void writeFrame({required MoqFrame frame}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtrackproducer_write_frame(
        uniffiClonePointer(),
        FfiConverterMoqFrame.lower(frame),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqTrackProducer {
  static MoqTrackProducer lift(Pointer<Void> ptr) {
    return MoqTrackProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqTrackProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqTrackProducer value) {
    return 8;
  }

  static LiftRetVal<MoqTrackProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqTrackProducer.lift(pointer), 8);
  }

  static int write(MoqTrackProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqTrackRequestInterface {
  void abort({required int errorCode});
  MoqTrackProducer accept({required MoqTrackInfo? info});
  MoqTrackDynamic dynamic_();
  String name();
}

final _MoqTrackRequestFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackrequest(ptr, status));
});

class MoqTrackRequest implements MoqTrackRequestInterface {
  late final Pointer<Void> _ptr;
  MoqTrackRequest._(this._ptr) {
    _MoqTrackRequestFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTrackRequest.lift(Pointer<Void> ptr) {
    return MoqTrackRequest._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtrackrequest(_ptr, status),
    );
  }

  void dispose() {
    _MoqTrackRequestFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackrequest(_ptr, status));
  }

  void abort({required int errorCode}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtrackrequest_abort(
        uniffiClonePointer(),
        FfiConverterUInt16.lower(errorCode),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqTrackProducer accept({required MoqTrackInfo? info}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackrequest_accept(
        uniffiClonePointer(),
        FfiConverterOptionalMoqTrackInfo.lower(info),
        status,
      ),
      FfiConverterMoqTrackProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqTrackDynamic dynamic_() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackrequest_dynamic(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDynamic.lift,
      moqExceptionErrorHandler,
    );
  }

  String name() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackrequest_name(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterString.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqTrackRequest {
  static MoqTrackRequest lift(Pointer<Void> ptr) {
    return MoqTrackRequest.lift(ptr);
  }

  static Pointer<Void> lower(MoqTrackRequest value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqTrackRequest value) {
    return 8;
  }

  static LiftRetVal<MoqTrackRequest> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqTrackRequest.lift(pointer), 8);
  }

  static int write(MoqTrackRequest value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqRequestInterface {
  Future<MoqSession> accept();
  void cancel();
  String path();
  String? query();
  Future<void> reject({required int code});
  void setConsume({required MoqOriginProducer? origin});
  void setPublish({required MoqOriginProducer? origin});
  String transport();
  String? url();
}

final _MoqRequestFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqrequest(ptr, status));
});

class MoqRequest implements MoqRequestInterface {
  late final Pointer<Void> _ptr;
  MoqRequest._(this._ptr) {
    _MoqRequestFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqRequest.lift(Pointer<Void> ptr) {
    return MoqRequest._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqrequest(_ptr, status),
    );
  }

  void dispose() {
    _MoqRequestFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqrequest(_ptr, status));
  }

  Future<MoqSession> accept() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqrequest_accept(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqSession.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqrequest_cancel(uniffiClonePointer(), status);
    }, null);
  }

  String path() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqrequest_path(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterString.lift,
      null,
    );
  }

  String? query() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqrequest_query(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalString.lift,
      null,
    );
  }

  Future<void> reject({required int code}) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqrequest_reject(
        uniffiClonePointer(),
        FfiConverterUInt16.lower(code),
      ),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  void setConsume({required MoqOriginProducer? origin}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqrequest_set_consume(
        uniffiClonePointer(),
        FfiConverterOptionalMoqOriginProducer.lower(origin),
        status,
      );
    }, null);
  }

  void setPublish({required MoqOriginProducer? origin}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqrequest_set_publish(
        uniffiClonePointer(),
        FfiConverterOptionalMoqOriginProducer.lower(origin),
        status,
      );
    }, null);
  }

  String transport() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqrequest_transport(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterString.lift,
      null,
    );
  }

  String? url() {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqrequest_url(uniffiClonePointer(), status),
      FfiConverterOptionalString.lift,
      null,
    );
  }
}

class FfiConverterMoqRequest {
  static MoqRequest lift(Pointer<Void> ptr) {
    return MoqRequest.lift(ptr);
  }

  static Pointer<Void> lower(MoqRequest value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqRequest value) {
    return 8;
  }

  static LiftRetVal<MoqRequest> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqRequest.lift(pointer), 8);
  }

  static int write(MoqRequest value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqServerInterface {
  Future<MoqRequest?> accept();
  void cancel();
  List<String> certFingerprints();
  Future<String> listen();
  void setBind({required String addr});
  void setConsume({required MoqOriginProducer? origin});
  void setPublish({required MoqOriginProducer? origin});
  void setTlsCert({required List<String> paths});
  void setTlsGenerate({required List<String> hostnames});
  void setTlsKey({required List<String> paths});
}

final _MoqServerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqserver(ptr, status));
});

class MoqServer implements MoqServerInterface {
  late final Pointer<Void> _ptr;
  MoqServer._(this._ptr) {
    _MoqServerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqServer()
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqserver_new(status),
        null,
      ) {
    _MoqServerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqServer.lift(Pointer<Void> ptr) {
    return MoqServer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqserver(_ptr, status),
    );
  }

  void dispose() {
    _MoqServerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqserver(_ptr, status));
  }

  Future<MoqRequest?> accept() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqserver_accept(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqRequest.lift,
      moqExceptionErrorHandler,
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqserver_cancel(uniffiClonePointer(), status);
    }, null);
  }

  List<String> certFingerprints() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqserver_cert_fingerprints(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterSequenceString.lift,
      moqExceptionErrorHandler,
    );
  }

  Future<String> listen() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqserver_listen(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterString.lift,
      moqExceptionErrorHandler,
    );
  }

  void setBind({required String addr}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqserver_set_bind(
        uniffiClonePointer(),
        FfiConverterString.lower(addr),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void setConsume({required MoqOriginProducer? origin}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqserver_set_consume(
        uniffiClonePointer(),
        FfiConverterOptionalMoqOriginProducer.lower(origin),
        status,
      );
    }, null);
  }

  void setPublish({required MoqOriginProducer? origin}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqserver_set_publish(
        uniffiClonePointer(),
        FfiConverterOptionalMoqOriginProducer.lower(origin),
        status,
      );
    }, null);
  }

  void setTlsCert({required List<String> paths}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqserver_set_tls_cert(
        uniffiClonePointer(),
        FfiConverterSequenceString.lower(paths),
        status,
      );
    }, null);
  }

  void setTlsGenerate({required List<String> hostnames}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqserver_set_tls_generate(
        uniffiClonePointer(),
        FfiConverterSequenceString.lower(hostnames),
        status,
      );
    }, null);
  }

  void setTlsKey({required List<String> paths}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqserver_set_tls_key(
        uniffiClonePointer(),
        FfiConverterSequenceString.lower(paths),
        status,
      );
    }, null);
  }
}

class FfiConverterMoqServer {
  static MoqServer lift(Pointer<Void> ptr) {
    return MoqServer.lift(ptr);
  }

  static Pointer<Void> lower(MoqServer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqServer value) {
    return 8;
  }

  static LiftRetVal<MoqServer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqServer.lift(pointer), 8);
  }

  static int write(MoqServer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqClientInterface {
  void cancel();
  Future<MoqSession> connect({required String url});
  void setBackoff({required MoqBackoff backoff});
  void setBind({required String addr});
  void setConsume({required MoqOriginProducer? origin});
  void setPublish({required MoqOriginProducer? origin});
  void setReconnect({required bool enabled});
  void setTlsCert({required String? path});
  void setTlsDisableVerify({required bool disable});
  void setTlsFingerprints({required List<String> fingerprints});
  void setTlsKey({required String? path});
  void setTlsRoots({required List<String> paths});
  void setTlsSystemRoots({required bool systemRoots});
}

final _MoqClientFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqclient(ptr, status));
});

class MoqClient implements MoqClientInterface {
  late final Pointer<Void> _ptr;
  MoqClient._(this._ptr) {
    _MoqClientFinalizer.attach(this, _ptr, detach: this);
  }
  MoqClient()
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqclient_new(status),
        null,
      ) {
    _MoqClientFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqClient.lift(Pointer<Void> ptr) {
    return MoqClient._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqclient(_ptr, status),
    );
  }

  void dispose() {
    _MoqClientFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqclient(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_cancel(uniffiClonePointer(), status);
    }, null);
  }

  Future<MoqSession> connect({required String url}) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqclient_connect(
        uniffiClonePointer(),
        FfiConverterString.lower(url),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqSession.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  void setBackoff({required MoqBackoff backoff}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_backoff(
        uniffiClonePointer(),
        FfiConverterMoqBackoff.lower(backoff),
        status,
      );
    }, null);
  }

  void setBind({required String addr}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_bind(
        uniffiClonePointer(),
        FfiConverterString.lower(addr),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void setConsume({required MoqOriginProducer? origin}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_consume(
        uniffiClonePointer(),
        FfiConverterOptionalMoqOriginProducer.lower(origin),
        status,
      );
    }, null);
  }

  void setPublish({required MoqOriginProducer? origin}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_publish(
        uniffiClonePointer(),
        FfiConverterOptionalMoqOriginProducer.lower(origin),
        status,
      );
    }, null);
  }

  void setReconnect({required bool enabled}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_reconnect(
        uniffiClonePointer(),
        FfiConverterBool.lower(enabled),
        status,
      );
    }, null);
  }

  void setTlsCert({required String? path}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_tls_cert(
        uniffiClonePointer(),
        FfiConverterOptionalString.lower(path),
        status,
      );
    }, null);
  }

  void setTlsDisableVerify({required bool disable}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_tls_disable_verify(
        uniffiClonePointer(),
        FfiConverterBool.lower(disable),
        status,
      );
    }, null);
  }

  void setTlsFingerprints({required List<String> fingerprints}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_tls_fingerprints(
        uniffiClonePointer(),
        FfiConverterSequenceString.lower(fingerprints),
        status,
      );
    }, null);
  }

  void setTlsKey({required String? path}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_tls_key(
        uniffiClonePointer(),
        FfiConverterOptionalString.lower(path),
        status,
      );
    }, null);
  }

  void setTlsRoots({required List<String> paths}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_tls_roots(
        uniffiClonePointer(),
        FfiConverterSequenceString.lower(paths),
        status,
      );
    }, null);
  }

  void setTlsSystemRoots({required bool systemRoots}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_set_tls_system_roots(
        uniffiClonePointer(),
        FfiConverterBool.lower(systemRoots),
        status,
      );
    }, null);
  }
}

class FfiConverterMoqClient {
  static MoqClient lift(Pointer<Void> ptr) {
    return MoqClient.lift(ptr);
  }

  static Pointer<Void> lower(MoqClient value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqClient value) {
    return 8;
  }

  static LiftRetVal<MoqClient> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqClient.lift(pointer), 8);
  }

  static int write(MoqClient value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqSessionInterface {
  void cancel({required int code});
  Future<void> closed();
  MoqOriginConsumer consumer();
  MoqOriginProducer publisher();
  void shutdown();
  MoqConnectionStats stats();
  Future<MoqConnectionStatus> status();
}

final _MoqSessionFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqsession(ptr, status));
});

class MoqSession implements MoqSessionInterface {
  late final Pointer<Void> _ptr;
  MoqSession._(this._ptr) {
    _MoqSessionFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqSession.lift(Pointer<Void> ptr) {
    return MoqSession._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqsession(_ptr, status),
    );
  }

  void dispose() {
    _MoqSessionFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqsession(_ptr, status));
  }

  void cancel({required int code}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqsession_cancel(
        uniffiClonePointer(),
        FfiConverterUInt32.lower(code),
        status,
      );
    }, null);
  }

  Future<void> closed() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqsession_closed(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  MoqOriginConsumer consumer() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_consumer(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqOriginConsumer.lift,
      null,
    );
  }

  MoqOriginProducer publisher() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_publisher(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqOriginProducer.lift,
      null,
    );
  }

  void shutdown() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqsession_shutdown(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  MoqConnectionStats stats() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_stats(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqConnectionStats.lift,
      null,
    );
  }

  Future<MoqConnectionStatus> status() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqsession_status(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterMoqConnectionStatus.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqSession {
  static MoqSession lift(Pointer<Void> ptr) {
    return MoqSession.lift(ptr);
  }

  static Pointer<Void> lower(MoqSession value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqSession value) {
    return 8;
  }

  static LiftRetVal<MoqSession> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqSession.lift(pointer), 8);
  }

  static int write(MoqSession value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

class FfiConverterBool {
  static bool lift(int value) {
    return value == 1;
  }

  static int lower(bool value) {
    return value ? 1 : 0;
  }

  static LiftRetVal<bool> read(Uint8List buf) {
    return LiftRetVal(FfiConverterBool.lift(buf.first), 1);
  }

  static RustBuffer lowerIntoRustBuffer(bool value) {
    return toRustBuffer(Uint8List.fromList([FfiConverterBool.lower(value)]));
  }

  static int allocationSize([bool value = false]) {
    return 1;
  }

  static int write(bool value, Uint8List buf) {
    buf.setAll(0, [value ? 1 : 0]);
    return allocationSize();
  }
}

class FfiConverterDouble64 {
  static double lift(double value) => value;
  static LiftRetVal<double> read(Uint8List buf) {
    return LiftRetVal(
      buf.buffer.asByteData(buf.offsetInBytes).getFloat64(0),
      8,
    );
  }

  static double lower(double value) => value;
  static int allocationSize([double value = 0]) {
    return 8;
  }

  static int write(double value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setFloat64(0, value);
    return FfiConverterDouble64.allocationSize();
  }
}

class FfiConverterMapStringToMoqAudio {
  static Map<String, MoqAudio> lift(RustBuffer buf) {
    return FfiConverterMapStringToMoqAudio.read(buf.asUint8List()).value;
  }

  static LiftRetVal<Map<String, MoqAudio>> read(Uint8List buf) {
    final map = <String, MoqAudio>{};
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < length; i++) {
      final k = FfiConverterString.read(Uint8List.view(buf.buffer, offset));
      offset += k.bytesRead;
      final v = FfiConverterMoqAudio.read(Uint8List.view(buf.buffer, offset));
      offset += v.bytesRead;
      map[k.value] = v.value;
    }
    return LiftRetVal(map, offset - buf.offsetInBytes);
  }

  static int write(Map<String, MoqAudio> value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    int offset = buf.offsetInBytes + 4;
    for (final entry in value.entries) {
      offset += FfiConverterString.write(
        entry.key,
        Uint8List.view(buf.buffer, offset),
      );
      offset += FfiConverterMoqAudio.write(
        entry.value,
        Uint8List.view(buf.buffer, offset),
      );
    }
    return offset - buf.offsetInBytes;
  }

  static int allocationSize(Map<String, MoqAudio> value) {
    return value.entries
        .map(
          (e) =>
              FfiConverterString.allocationSize(e.key) +
              FfiConverterMoqAudio.allocationSize(e.value),
        )
        .fold(4, (a, b) => a + b);
  }

  static RustBuffer lower(Map<String, MoqAudio> value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }
}

class FfiConverterMapStringToMoqVideo {
  static Map<String, MoqVideo> lift(RustBuffer buf) {
    return FfiConverterMapStringToMoqVideo.read(buf.asUint8List()).value;
  }

  static LiftRetVal<Map<String, MoqVideo>> read(Uint8List buf) {
    final map = <String, MoqVideo>{};
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < length; i++) {
      final k = FfiConverterString.read(Uint8List.view(buf.buffer, offset));
      offset += k.bytesRead;
      final v = FfiConverterMoqVideo.read(Uint8List.view(buf.buffer, offset));
      offset += v.bytesRead;
      map[k.value] = v.value;
    }
    return LiftRetVal(map, offset - buf.offsetInBytes);
  }

  static int write(Map<String, MoqVideo> value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    int offset = buf.offsetInBytes + 4;
    for (final entry in value.entries) {
      offset += FfiConverterString.write(
        entry.key,
        Uint8List.view(buf.buffer, offset),
      );
      offset += FfiConverterMoqVideo.write(
        entry.value,
        Uint8List.view(buf.buffer, offset),
      );
    }
    return offset - buf.offsetInBytes;
  }

  static int allocationSize(Map<String, MoqVideo> value) {
    return value.entries
        .map(
          (e) =>
              FfiConverterString.allocationSize(e.key) +
              FfiConverterMoqVideo.allocationSize(e.value),
        )
        .fold(4, (a, b) => a + b);
  }

  static RustBuffer lower(Map<String, MoqVideo> value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }
}

class FfiConverterMapStringToString {
  static Map<String, String> lift(RustBuffer buf) {
    return FfiConverterMapStringToString.read(buf.asUint8List()).value;
  }

  static LiftRetVal<Map<String, String>> read(Uint8List buf) {
    final map = <String, String>{};
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < length; i++) {
      final k = FfiConverterString.read(Uint8List.view(buf.buffer, offset));
      offset += k.bytesRead;
      final v = FfiConverterString.read(Uint8List.view(buf.buffer, offset));
      offset += v.bytesRead;
      map[k.value] = v.value;
    }
    return LiftRetVal(map, offset - buf.offsetInBytes);
  }

  static int write(Map<String, String> value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    int offset = buf.offsetInBytes + 4;
    for (final entry in value.entries) {
      offset += FfiConverterString.write(
        entry.key,
        Uint8List.view(buf.buffer, offset),
      );
      offset += FfiConverterString.write(
        entry.value,
        Uint8List.view(buf.buffer, offset),
      );
    }
    return offset - buf.offsetInBytes;
  }

  static int allocationSize(Map<String, String> value) {
    return value.entries
        .map(
          (e) =>
              FfiConverterString.allocationSize(e.key) +
              FfiConverterString.allocationSize(e.value),
        )
        .fold(4, (a, b) => a + b);
  }

  static RustBuffer lower(Map<String, String> value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }
}

class FfiConverterOptionalBool {
  static bool? lift(RustBuffer buf) {
    return FfiConverterOptionalBool.read(buf.asUint8List()).value;
  }

  static LiftRetVal<bool?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterBool.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<bool?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([bool? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterBool.allocationSize(value) + 1;
  }

  static RustBuffer lower(bool? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalBool.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalBool.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(bool? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterBool.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalDouble64 {
  static double? lift(RustBuffer buf) {
    return FfiConverterOptionalDouble64.read(buf.asUint8List()).value;
  }

  static LiftRetVal<double?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterDouble64.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<double?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([double? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterDouble64.allocationSize(value) + 1;
  }

  static RustBuffer lower(double? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalDouble64.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalDouble64.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(double? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterDouble64.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqAnnouncement {
  static MoqAnnouncement? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqAnnouncement.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAnnouncement?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqAnnouncement.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqAnnouncement?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqAnnouncement? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqAnnouncement.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqAnnouncement? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqAnnouncement.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqAnnouncement.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqAnnouncement? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqAnnouncement.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqCatalog {
  static MoqCatalog? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqCatalog.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqCatalog?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqCatalog.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqCatalog?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqCatalog? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqCatalog.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqCatalog? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqCatalog.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqCatalog.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqCatalog? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqCatalog.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqDatagram {
  static MoqDatagram? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqDatagram.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqDatagram?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqDatagram.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqDatagram?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqDatagram? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqDatagram.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqDatagram? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqDatagram.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqDatagram.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqDatagram? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqDatagram.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqDimensions {
  static MoqDimensions? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqDimensions.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqDimensions?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqDimensions.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqDimensions?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqDimensions? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqDimensions.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqDimensions? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqDimensions.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqDimensions.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqDimensions? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqDimensions.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqFetchGroupOptions {
  static MoqFetchGroupOptions? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqFetchGroupOptions.read(
      buf.asUint8List(),
    ).value;
  }

  static LiftRetVal<MoqFetchGroupOptions?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqFetchGroupOptions.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqFetchGroupOptions?>(
      result.value,
      result.bytesRead + 1,
    );
  }

  static int allocationSize([MoqFetchGroupOptions? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqFetchGroupOptions.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqFetchGroupOptions? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqFetchGroupOptions.allocationSize(
      value,
    );
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqFetchGroupOptions.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqFetchGroupOptions? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqFetchGroupOptions.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqFrame {
  static MoqFrame? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqFrame.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqFrame?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqFrame.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqFrame?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqFrame? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqFrame.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqFrame? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqFrame.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqFrame.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqFrame? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqFrame.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqGroupConsumer {
  static MoqGroupConsumer? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqGroupConsumer.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqGroupConsumer?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqGroupConsumer.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqGroupConsumer?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqGroupConsumer? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqGroupConsumer.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqGroupConsumer? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqGroupConsumer.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqGroupConsumer.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqGroupConsumer? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqGroupConsumer.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqMediaFrame {
  static MoqMediaFrame? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqMediaFrame.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqMediaFrame?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqMediaFrame.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqMediaFrame?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqMediaFrame? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqMediaFrame.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqMediaFrame? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqMediaFrame.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqMediaFrame.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqMediaFrame? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqMediaFrame.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqOriginProducer {
  static MoqOriginProducer? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqOriginProducer.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqOriginProducer?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqOriginProducer.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqOriginProducer?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqOriginProducer? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqOriginProducer.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqOriginProducer? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqOriginProducer.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqOriginProducer.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqOriginProducer? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqOriginProducer.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqRequest {
  static MoqRequest? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqRequest.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqRequest?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqRequest.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqRequest?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqRequest? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqRequest.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqRequest? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqRequest.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqRequest.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqRequest? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqRequest.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqSubscription {
  static MoqSubscription? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqSubscription.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqSubscription?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqSubscription.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqSubscription?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqSubscription? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqSubscription.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqSubscription? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqSubscription.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqSubscription.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqSubscription? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqSubscription.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqTrackInfo {
  static MoqTrackInfo? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqTrackInfo.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqTrackInfo?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqTrackInfo.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqTrackInfo?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqTrackInfo? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqTrackInfo.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqTrackInfo? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqTrackInfo.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqTrackInfo.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqTrackInfo? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqTrackInfo.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqVideoHint {
  static MoqVideoHint? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqVideoHint.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoHint?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqVideoHint.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqVideoHint?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqVideoHint? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqVideoHint.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqVideoHint? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalMoqVideoHint.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalMoqVideoHint.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(MoqVideoHint? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqVideoHint.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalString {
  static String? lift(RustBuffer buf) {
    return FfiConverterOptionalString.read(buf.asUint8List()).value;
  }

  static LiftRetVal<String?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterString.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<String?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([String? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterString.allocationSize(value) + 1;
  }

  static RustBuffer lower(String? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalString.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalString.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(String? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterString.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalUInt64 {
  static int? lift(RustBuffer buf) {
    return FfiConverterOptionalUInt64.read(buf.asUint8List()).value;
  }

  static LiftRetVal<int?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<int?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([int? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterUInt64.allocationSize(value) + 1;
  }

  static RustBuffer lower(int? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalUInt64.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalUInt64.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(int? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterUInt64.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalUint8List {
  static Uint8List? lift(RustBuffer buf) {
    return FfiConverterOptionalUint8List.read(buf.asUint8List()).value;
  }

  static LiftRetVal<Uint8List?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<Uint8List?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([Uint8List? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterUint8List.allocationSize(value) + 1;
  }

  static RustBuffer lower(Uint8List? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final length = FfiConverterOptionalUint8List.allocationSize(value);
    final Pointer<Uint8> frameData = calloc<Uint8>(length);
    final buf = frameData.asTypedList(length);
    FfiConverterOptionalUint8List.write(value, buf);
    final bytes = calloc<ForeignBytes>();
    bytes.ref.len = length;
    bytes.ref.data = frameData;
    return RustBuffer.fromBytes(bytes.ref);
  }

  static int write(Uint8List? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterUint8List.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterSequenceString {
  static List<String> lift(RustBuffer buf) {
    return FfiConverterSequenceString.read(buf.asUint8List()).value;
  }

  static LiftRetVal<List<String>> read(Uint8List buf) {
    List<String> res = [];
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < length; i++) {
      final ret = FfiConverterString.read(Uint8List.view(buf.buffer, offset));
      offset += ret.bytesRead;
      res.add(ret.value);
    }
    return LiftRetVal(res, offset - buf.offsetInBytes);
  }

  static int write(List<String> value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < value.length; i++) {
      offset += FfiConverterString.write(
        value[i],
        Uint8List.view(buf.buffer, offset),
      );
    }
    return offset - buf.offsetInBytes;
  }

  static int allocationSize(List<String> value) {
    return value
            .map((l) => FfiConverterString.allocationSize(l))
            .fold(0, (a, b) => a + b) +
        4;
  }

  static RustBuffer lower(List<String> value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }
}

class FfiConverterSequenceUInt64 {
  static List<int> lift(RustBuffer buf) {
    return FfiConverterSequenceUInt64.read(buf.asUint8List()).value;
  }

  static LiftRetVal<List<int>> read(Uint8List buf) {
    List<int> res = [];
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < length; i++) {
      final ret = FfiConverterUInt64.read(Uint8List.view(buf.buffer, offset));
      offset += ret.bytesRead;
      res.add(ret.value);
    }
    return LiftRetVal(res, offset - buf.offsetInBytes);
  }

  static int write(List<int> value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < value.length; i++) {
      offset += FfiConverterUInt64.write(
        value[i],
        Uint8List.view(buf.buffer, offset),
      );
    }
    return offset - buf.offsetInBytes;
  }

  static int allocationSize(List<int> value) {
    return value
            .map((l) => FfiConverterUInt64.allocationSize(l))
            .fold(0, (a, b) => a + b) +
        4;
  }

  static RustBuffer lower(List<int> value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }
}

class FfiConverterUInt16 {
  static int lift(int value) => value;
  static LiftRetVal<int> read(Uint8List buf) {
    return LiftRetVal(buf.buffer.asByteData(buf.offsetInBytes).getUint16(0), 2);
  }

  static int lower(int value) {
    if (value < 0 || value > 65535) {
      throw ArgumentError("Value out of range for u16: " + value.toString());
    }
    return value;
  }

  static int allocationSize([int value = 0]) {
    return 2;
  }

  static int write(int value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setUint16(0, lower(value));
    return 2;
  }
}

class FfiConverterUInt32 {
  static int lift(int value) => value;
  static LiftRetVal<int> read(Uint8List buf) {
    return LiftRetVal(buf.buffer.asByteData(buf.offsetInBytes).getUint32(0), 4);
  }

  static int lower(int value) {
    if (value < 0 || value > 4294967295) {
      throw ArgumentError("Value out of range for u32: " + value.toString());
    }
    return value;
  }

  static int allocationSize([int value = 0]) {
    return 4;
  }

  static int write(int value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setUint32(0, lower(value));
    return 4;
  }
}

class FfiConverterUInt64 {
  static int lift(int value) => value;
  static LiftRetVal<int> read(Uint8List buf) {
    return LiftRetVal(buf.buffer.asByteData(buf.offsetInBytes).getUint64(0), 8);
  }

  static int lower(int value) {
    if (value < 0) {
      throw ArgumentError("Value out of range for u64: " + value.toString());
    }
    return value;
  }

  static int allocationSize([int value = 0]) {
    return 8;
  }

  static int write(int value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setUint64(0, lower(value));
    return 8;
  }
}

class FfiConverterUInt8 {
  static int lift(int value) => value;
  static LiftRetVal<int> read(Uint8List buf) {
    return LiftRetVal(buf.buffer.asByteData(buf.offsetInBytes).getUint8(0), 1);
  }

  static int lower(int value) {
    if (value < 0 || value > 255) {
      throw ArgumentError("Value out of range for u8: " + value.toString());
    }
    return value;
  }

  static int allocationSize([int value = 0]) {
    return 1;
  }

  static int write(int value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setUint8(0, lower(value));
    return 1;
  }
}

class FfiConverterUint8List {
  static Uint8List lift(RustBuffer value) {
    return FfiConverterUint8List.read(value.asUint8List()).value;
  }

  static LiftRetVal<Uint8List> read(Uint8List buf) {
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final bytes = Uint8List.view(buf.buffer, buf.offsetInBytes + 4, length);
    return LiftRetVal(bytes, length + 4);
  }

  static RustBuffer lower(Uint8List value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int allocationSize([Uint8List? value]) {
    if (value == null) {
      return 4;
    }
    return 4 + value.length;
  }

  static int write(Uint8List value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    buf.setRange(4, 4 + value.length, value);
    return 4 + value.length;
  }
}

const _uniffiAssetId = "package:moq_ffi/uniffi:moq_ffi";
void moqLogLevel({required String level}) {
  return rustCall((status) {
    uniffi_moq_ffi_fn_func_moq_log_level(
      FfiConverterString.lower(level),
      status,
    );
  }, moqExceptionErrorHandler);
}

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqbroadcastconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqbroadcastconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer, Uint64, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastconsumer_fetch_group(
  Pointer<Void> ptr,
  RustBuffer name,
  int sequence,
  RustBuffer options,
);

@Native<
  Pointer<Void> Function(
    Pointer<Void>,
    RustBuffer,
    Uint64,
    RustBuffer,
    RustBuffer,
  )
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastconsumer_fetch_media_group(
  Pointer<Void> ptr,
  RustBuffer name,
  int sequence,
  RustBuffer container,
  RustBuffer options,
);

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqbroadcastconsumer_resolve(
  Pointer<Void> ptr,
  RustBuffer reference,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_catalog(
  Pointer<Void> ptr,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, RustBuffer, RustBuffer)
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_media(
  Pointer<Void> ptr,
  RustBuffer name,
  RustBuffer container,
  RustBuffer subscription,
);

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_track(
  Pointer<Void> ptr,
  RustBuffer name,
  RustBuffer subscription,
);

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_json_snapshot(
  Pointer<Void> ptr,
  RustBuffer name,
  RustBuffer config,
);

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastconsumer_subscribe_json_stream(
  Pointer<Void> ptr,
  RustBuffer name,
  RustBuffer config,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqcatalogconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqcatalogconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqcatalogconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqcatalogconsumer_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqgroupconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqgroupconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqgroupconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqgroupconsumer_read_frame(
  Pointer<Void> ptr,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqgroupconsumer_sequence(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediaconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediaconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediaconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqmediaconsumer_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediagroupconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediagroupconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediagroupconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqmediagroupconsumer_next(
  Pointer<Void> ptr,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqmediagroupconsumer_sequence(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqtrackconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqtrackconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtrackconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqtrackconsumer_info(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackconsumer_next_group(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackconsumer_read_frame(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackconsumer_recv_datagram(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackconsumer_recv_group(
  Pointer<Void> ptr,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtrackconsumer_update(
  Pointer<Void> ptr,
  RustBuffer subscription,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqjsonsnapshotconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqjsonsnapshotconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonsnapshotconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqjsonsnapshotconsumer_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqjsonsnapshotproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqjsonsnapshotproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonsnapshotproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonsnapshotproducer_update(
  Pointer<Void> ptr,
  RustBuffer value,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqjsonstreamconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqjsonstreamconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonstreamconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqjsonstreamconsumer_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqjsonstreamproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqjsonstreamproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonstreamproducer_append(
  Pointer<Void> ptr,
  RustBuffer value,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonstreamproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqannounce(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqannounce(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqannounce_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqannounce_update(
  Pointer<Void> ptr,
  RustBuffer route,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqannounced(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqannounced(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqannounced_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqannounced_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqannouncedbroadcast(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqannouncedbroadcast(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqannouncedbroadcast_available(
  Pointer<Void> ptr,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqannouncedbroadcast_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqannouncement(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqannouncement(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Int8 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqannouncement_active(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqannouncement_path(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqannouncement_route(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqbroadcastrequest(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqbroadcastrequest(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint16, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqbroadcastrequest_abort(
  Pointer<Void> ptr,
  int error_code,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqbroadcastrequest_accept(
  Pointer<Void> ptr,
  Pointer<Void> broadcast,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqbroadcastrequest_path(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqoriginconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqoriginconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqoriginconsumer_announced(
  Pointer<Void> ptr,
  RustBuffer prefix,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqoriginconsumer_announced_broadcast(
  Pointer<Void> ptr,
  RustBuffer path,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqoriginconsumer_request_broadcast(
  Pointer<Void> ptr,
  RustBuffer path,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqorigindynamic(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqorigindynamic(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqorigindynamic_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqorigindynamic_requested_broadcast(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqoriginproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqoriginproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqoriginproducer_new(
  RustBuffer options,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(
    Pointer<Void>,
    RustBuffer,
    RustBuffer,
    Pointer<RustCallStatus>,
  )
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqoriginproducer_announce(
  Pointer<Void> ptr,
  RustBuffer prefix,
  RustBuffer route,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqoriginproducer_consume(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqoriginproducer_create_broadcast(
  Pointer<Void> ptr,
  RustBuffer path,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqoriginproducer_dynamic(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqbroadcastdynamic(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqbroadcastdynamic(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqbroadcastdynamic_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastdynamic_requested_track(Pointer<Void> ptr);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqbroadcastproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqbroadcastproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqbroadcastproducer_new(
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(
    Pointer<Void>,
    RustBuffer,
    RustBuffer,
    Pointer<RustCallStatus>,
  )
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_json_snapshot(
  Pointer<Void> ptr,
  RustBuffer name,
  RustBuffer config,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(
    Pointer<Void>,
    RustBuffer,
    RustBuffer,
    Pointer<RustCallStatus>,
  )
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_json_stream(
  Pointer<Void> ptr,
  RustBuffer name,
  RustBuffer config,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqbroadcastproducer_consume(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqbroadcastproducer_dynamic(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqbroadcastproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_audio(
  Pointer<Void> ptr,
  RustBuffer init,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(
    Pointer<Void>,
    Pointer<Void>,
    RustBuffer,
    Pointer<RustCallStatus>,
  )
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_audio_on_track(
  Pointer<Void> ptr,
  Pointer<Void> request,
  RustBuffer init,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_container(
  Pointer<Void> ptr,
  RustBuffer init,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_container_stream(
  Pointer<Void> ptr,
  RustBuffer format,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(
    Pointer<Void>,
    RustBuffer,
    RustBuffer,
    Pointer<RustCallStatus>,
  )
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_track(
  Pointer<Void> ptr,
  RustBuffer name,
  RustBuffer info,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_video(
  Pointer<Void> ptr,
  RustBuffer init,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(
    Pointer<Void>,
    Pointer<Void>,
    RustBuffer,
    Pointer<RustCallStatus>,
  )
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_video_on_track(
  Pointer<Void> ptr,
  Pointer<Void> request,
  RustBuffer init,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastproducer_publish_video_stream(
  Pointer<Void> ptr,
  RustBuffer init,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void
uniffi_moq_ffi_fn_method_moqbroadcastproducer_remove_catalog_section(
  Pointer<Void> ptr,
  RustBuffer name,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Int8, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqbroadcastproducer_set_announce(
  Pointer<Void> ptr,
  int announce,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(Pointer<Void>, RustBuffer, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external void uniffi_moq_ffi_fn_method_moqbroadcastproducer_set_catalog_section(
  Pointer<Void> ptr,
  RustBuffer name,
  RustBuffer json,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void
uniffi_moq_ffi_fn_method_moqbroadcastproducer_set_video_properties(
  Pointer<Void> ptr,
  RustBuffer properties,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqcontainerproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqcontainerproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqcontainerproducer_cut(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqcontainerproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqcontainerproducer_seek(
  Pointer<Void> ptr,
  int sequence,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqcontainerproducer_write(
  Pointer<Void> ptr,
  RustBuffer payload,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqcontainerstreamproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqcontainerstreamproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqcontainerstreamproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqcontainerstreamproducer_write(
  Pointer<Void> ptr,
  RustBuffer payload,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqgroupproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqgroupproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint16, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqgroupproducer_abort(
  Pointer<Void> ptr,
  int error_code,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqgroupproducer_consume(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqgroupproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqgroupproducer_sequence(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqgroupproducer_write_frame(
  Pointer<Void> ptr,
  RustBuffer frame,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqgrouprequest(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqgrouprequest(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint16, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqgrouprequest_abort(
  Pointer<Void> ptr,
  int error_code,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqgrouprequest_accept(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint8 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqgrouprequest_priority(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqgrouprequest_sequence(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediaproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediaproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediaproducer_cut(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediaproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqmediaproducer_name(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediaproducer_seek(
  Pointer<Void> ptr,
  int sequence,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqmediaproducer_unused(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqmediaproducer_used(
  Pointer<Void> ptr,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediaproducer_write_frame(
  Pointer<Void> ptr,
  RustBuffer frame,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediastreamproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediastreamproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediastreamproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediastreamproducer_write(
  Pointer<Void> ptr,
  RustBuffer payload,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqtrackdynamic(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqtrackdynamic(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtrackdynamic_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackdynamic_requested_group(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqtrackproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqtrackproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint16, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtrackproducer_abort(
  Pointer<Void> ptr,
  int error_code,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint64 Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqtrackproducer_append_datagram(
  Pointer<Void> ptr,
  RustBuffer frame,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackproducer_append_group(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackproducer_consume(
  Pointer<Void> ptr,
  RustBuffer subscription,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackproducer_create_group(
  Pointer<Void> ptr,
  int sequence,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackproducer_dynamic(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtrackproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtrackproducer_finish_at(
  Pointer<Void> ptr,
  int final_sequence,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqtrackproducer_name(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackproducer_unused(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackproducer_used(
  Pointer<Void> ptr,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtrackproducer_write_frame(
  Pointer<Void> ptr,
  RustBuffer frame,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqtrackrequest(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqtrackrequest(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint16, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtrackrequest_abort(
  Pointer<Void> ptr,
  int error_code,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackrequest_accept(
  Pointer<Void> ptr,
  RustBuffer info,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackrequest_dynamic(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqtrackrequest_name(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqrequest(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqrequest(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqrequest_accept(
  Pointer<Void> ptr,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqrequest_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqrequest_path(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqrequest_query(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Uint16)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqrequest_reject(
  Pointer<Void> ptr,
  int code,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqrequest_set_consume(
  Pointer<Void> ptr,
  RustBuffer origin,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqrequest_set_publish(
  Pointer<Void> ptr,
  RustBuffer origin,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqrequest_transport(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqrequest_url(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqserver(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqserver(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqserver_new(
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqserver_accept(
  Pointer<Void> ptr,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqserver_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqserver_cert_fingerprints(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqserver_listen(
  Pointer<Void> ptr,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqserver_set_bind(
  Pointer<Void> ptr,
  RustBuffer addr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqserver_set_consume(
  Pointer<Void> ptr,
  RustBuffer origin,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqserver_set_publish(
  Pointer<Void> ptr,
  RustBuffer origin,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqserver_set_tls_cert(
  Pointer<Void> ptr,
  RustBuffer paths,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqserver_set_tls_generate(
  Pointer<Void> ptr,
  RustBuffer hostnames,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqserver_set_tls_key(
  Pointer<Void> ptr,
  RustBuffer paths,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqclient(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqclient(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqclient_new(
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqclient_connect(
  Pointer<Void> ptr,
  RustBuffer url,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_backoff(
  Pointer<Void> ptr,
  RustBuffer backoff,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_bind(
  Pointer<Void> ptr,
  RustBuffer addr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_consume(
  Pointer<Void> ptr,
  RustBuffer origin,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_publish(
  Pointer<Void> ptr,
  RustBuffer origin,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Int8, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_reconnect(
  Pointer<Void> ptr,
  int enabled,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_tls_cert(
  Pointer<Void> ptr,
  RustBuffer path,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Int8, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_tls_disable_verify(
  Pointer<Void> ptr,
  int disable,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_tls_fingerprints(
  Pointer<Void> ptr,
  RustBuffer fingerprints,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_tls_key(
  Pointer<Void> ptr,
  RustBuffer path,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_tls_roots(
  Pointer<Void> ptr,
  RustBuffer paths,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Int8, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqclient_set_tls_system_roots(
  Pointer<Void> ptr,
  int system_roots,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqsession(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqsession(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint32, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqsession_cancel(
  Pointer<Void> ptr,
  int code,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqsession_closed(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqsession_consumer(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqsession_publisher(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqsession_shutdown(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqsession_stats(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqsession_status(
  Pointer<Void> ptr,
);

@Native<Void Function(RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_func_moq_log_level(
  RustBuffer level,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_u8(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_u8(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_u8(Pointer<Void> handle);

@Native<Uint8 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int ffi_moq_ffi_rust_future_complete_u8(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_i8(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_i8(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_i8(Pointer<Void> handle);

@Native<Int8 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int ffi_moq_ffi_rust_future_complete_i8(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_u16(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_u16(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_u16(Pointer<Void> handle);

@Native<Uint16 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int ffi_moq_ffi_rust_future_complete_u16(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_i16(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_i16(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_i16(Pointer<Void> handle);

@Native<Int16 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int ffi_moq_ffi_rust_future_complete_i16(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_u32(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_u32(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_u32(Pointer<Void> handle);

@Native<Uint32 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int ffi_moq_ffi_rust_future_complete_u32(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_i32(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_i32(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_i32(Pointer<Void> handle);

@Native<Int32 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int ffi_moq_ffi_rust_future_complete_i32(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_u64(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_u64(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_u64(Pointer<Void> handle);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int ffi_moq_ffi_rust_future_complete_u64(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_i64(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_i64(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_i64(Pointer<Void> handle);

@Native<Int64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int ffi_moq_ffi_rust_future_complete_i64(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_f32(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_f32(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_f32(Pointer<Void> handle);

@Native<Float Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external double ffi_moq_ffi_rust_future_complete_f32(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_f64(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_f64(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_f64(Pointer<Void> handle);

@Native<Double Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external double ffi_moq_ffi_rust_future_complete_f64(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_rust_buffer(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_rust_buffer(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_rust_buffer(Pointer<Void> handle);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer ffi_moq_ffi_rust_future_complete_rust_buffer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(
    Pointer<Void>,
    Pointer<NativeFunction<UniffiRustFutureContinuationCallback>>,
    Pointer<Void>,
  )
>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_poll_void(
  Pointer<Void> handle,
  Pointer<NativeFunction<UniffiRustFutureContinuationCallback>> callback,
  Pointer<Void> callback_data,
);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_cancel_void(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external void ffi_moq_ffi_rust_future_free_void(Pointer<Void> handle);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void ffi_moq_ffi_rust_future_complete_void(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_func_moq_log_level();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_fetch_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_fetch_media_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_resolve();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_catalog();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_media();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_track();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_json_snapshot();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_json_stream();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqcatalogconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqcatalogconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupconsumer_read_frame();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupconsumer_sequence();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediaconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediaconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_sequence();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackconsumer_info();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackconsumer_next_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackconsumer_read_frame();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackconsumer_recv_datagram();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackconsumer_recv_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackconsumer_update();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonsnapshotproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonsnapshotproducer_update();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonstreamconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonstreamconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonstreamproducer_append();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonstreamproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannounce_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannounce_update();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannounced_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannounced_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_available();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannouncement_active();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannouncement_path();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannouncement_route();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastrequest_abort();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastrequest_accept();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastrequest_path();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqoriginconsumer_request_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqorigindynamic_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqorigindynamic_requested_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqoriginproducer_announce();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqoriginproducer_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqoriginproducer_create_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqoriginproducer_dynamic();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastdynamic_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastdynamic_requested_track();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_json_snapshot();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_json_stream();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastproducer_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastproducer_dynamic();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_audio();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_audio_on_track();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_container();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_container_stream();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_track();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_video();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_video_on_track();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_video_stream();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_remove_catalog_section();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastproducer_set_announce();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_set_catalog_section();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_set_video_properties();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqcontainerproducer_cut();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqcontainerproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqcontainerproducer_seek();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqcontainerproducer_write();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqcontainerstreamproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqcontainerstreamproducer_write();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupproducer_abort();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupproducer_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupproducer_sequence();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupproducer_write_frame();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgrouprequest_abort();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgrouprequest_accept();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgrouprequest_priority();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgrouprequest_sequence();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediaproducer_cut();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediaproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediaproducer_name();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediaproducer_seek();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediaproducer_unused();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediaproducer_used();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediaproducer_write_frame();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediastreamproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediastreamproducer_write();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackdynamic_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackdynamic_requested_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_abort();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_append_datagram();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_append_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_create_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_dynamic();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_finish_at();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_name();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_unused();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_used();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_write_frame();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequest_abort();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequest_accept();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequest_dynamic();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequest_name();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_accept();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_path();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_query();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_reject();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_set_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_set_publish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_transport();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_url();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_accept();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_cert_fingerprints();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_listen();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_set_bind();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_set_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_set_publish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_set_tls_cert();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_set_tls_generate();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqserver_set_tls_key();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_connect();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_backoff();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_bind();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_publish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_reconnect();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_tls_cert();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_tls_disable_verify();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_tls_fingerprints();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_tls_key();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_tls_roots();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_set_tls_system_roots();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_closed();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_consumer();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_publisher();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_shutdown();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_stats();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_status();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqoriginproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqbroadcastproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqserver_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqclient_new();

@Native<Uint32 Function()>(assetId: _uniffiAssetId)
external int ffi_moq_ffi_uniffi_contract_version();

void _checkApiVersion() {
  final bindingsVersion = 30;
  final scaffoldingVersion = ffi_moq_ffi_uniffi_contract_version();
  if (bindingsVersion != scaffoldingVersion) {
    throw UniffiInternalError.panicked(
      "UniFFI contract version mismatch: bindings version \$bindingsVersion, scaffolding version \$scaffoldingVersion",
    );
  }
}

void _checkApiChecksums() {
  if (uniffi_moq_ffi_checksum_func_moq_log_level() != 24625) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_fetch_group() !=
      18633) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_fetch_media_group() !=
      40237) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_resolve() != 55875) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_catalog() !=
      34722) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_media() !=
      33930) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_track() !=
      2348) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_json_snapshot() !=
      46473) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_json_stream() !=
      3028) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqcatalogconsumer_cancel() != 65421) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqcatalogconsumer_next() != 33133) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupconsumer_cancel() != 52548) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupconsumer_read_frame() != 26363) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupconsumer_sequence() != 46527) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediaconsumer_cancel() != 14280) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediaconsumer_next() != 42389) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_cancel() != 47486) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_next() != 22636) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_sequence() !=
      22332) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_cancel() != 65022) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_info() != 46426) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_next_group() != 42933) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_read_frame() != 15170) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_recv_datagram() !=
      46985) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_recv_group() != 60887) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_update() != 24851) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_cancel() !=
      45114) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_next() != 64727) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotproducer_finish() !=
      42593) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotproducer_update() !=
      18037) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonstreamconsumer_cancel() != 29308) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonstreamconsumer_next() != 7523) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonstreamproducer_append() != 12571) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonstreamproducer_finish() != 51459) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannounce_cancel() != 2116) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannounce_update() != 49327) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannounced_cancel() != 43666) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannounced_next() != 4806) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_available() !=
      13508) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_cancel() != 63175) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannouncement_active() != 44744) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannouncement_path() != 57757) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannouncement_route() != 55351) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastrequest_abort() != 42319) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastrequest_accept() != 36946) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastrequest_path() != 6534) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced() != 15694) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced_broadcast() !=
      19912) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginconsumer_request_broadcast() !=
      37085) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqorigindynamic_cancel() != 43442) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqorigindynamic_requested_broadcast() !=
      26471) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginproducer_announce() != 29084) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginproducer_consume() != 52357) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginproducer_create_broadcast() !=
      35572) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginproducer_dynamic() != 40207) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastdynamic_cancel() != 25875) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastdynamic_requested_track() !=
      24118) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_json_snapshot() !=
      51036) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_json_stream() !=
      47317) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_consume() != 27634) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_dynamic() != 55635) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_finish() != 7183) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_audio() !=
      47444) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_audio_on_track() !=
      33897) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_container() !=
      24539) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_container_stream() !=
      11217) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_track() !=
      44452) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_video() !=
      16383) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_video_on_track() !=
      60666) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_video_stream() !=
      28640) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_remove_catalog_section() !=
      8608) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_set_announce() !=
      15288) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_set_catalog_section() !=
      25735) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_set_video_properties() !=
      9178) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqcontainerproducer_cut() != 17534) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqcontainerproducer_finish() != 13064) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqcontainerproducer_seek() != 61349) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqcontainerproducer_write() != 13274) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqcontainerstreamproducer_finish() !=
      29733) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqcontainerstreamproducer_write() !=
      18446) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_abort() != 59787) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_consume() != 53274) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_finish() != 35444) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_sequence() != 21067) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_write_frame() != 2442) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgrouprequest_abort() != 26970) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgrouprequest_accept() != 48242) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgrouprequest_priority() != 1745) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgrouprequest_sequence() != 29523) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediaproducer_cut() != 58543) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediaproducer_finish() != 38480) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediaproducer_name() != 7199) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediaproducer_seek() != 43157) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediaproducer_unused() != 35935) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediaproducer_used() != 53654) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediaproducer_write_frame() != 7321) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediastreamproducer_finish() != 2732) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediastreamproducer_write() != 31109) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackdynamic_cancel() != 57913) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackdynamic_requested_group() !=
      63983) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_abort() != 37537) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_append_datagram() !=
      6272) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_append_group() != 45225) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_consume() != 30970) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_create_group() != 38978) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_dynamic() != 58584) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_finish() != 16707) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_finish_at() != 24581) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_name() != 14598) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_unused() != 9025) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_used() != 36898) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_write_frame() != 18663) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequest_abort() != 62713) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequest_accept() != 47766) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequest_dynamic() != 24801) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequest_name() != 56715) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_accept() != 46183) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_cancel() != 25859) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_path() != 48052) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_query() != 23842) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_reject() != 57471) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_set_consume() != 10143) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_set_publish() != 48930) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_transport() != 5942) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_url() != 34138) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_accept() != 62476) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_cancel() != 25785) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_cert_fingerprints() != 32082) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_listen() != 9040) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_set_bind() != 60575) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_set_consume() != 29005) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_set_publish() != 54637) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_set_tls_cert() != 6344) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_set_tls_generate() != 51810) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_set_tls_key() != 61191) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_cancel() != 29949) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_connect() != 52298) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_backoff() != 28024) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_bind() != 7248) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_consume() != 64342) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_publish() != 29680) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_reconnect() != 24915) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_tls_cert() != 24223) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_tls_disable_verify() !=
      58510) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_tls_fingerprints() !=
      48211) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_tls_key() != 499) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_tls_roots() != 46542) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_set_tls_system_roots() !=
      24617) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_cancel() != 39476) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_closed() != 7901) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_consumer() != 62364) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_publisher() != 55435) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_shutdown() != 820) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_stats() != 44305) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_status() != 49725) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqoriginproducer_new() != 54724) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqbroadcastproducer_new() != 37572) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqserver_new() != 42979) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqclient_new() != 44907) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
}

void ensureInitialized() {
  _checkApiVersion();
  _checkApiChecksums();
}

@Deprecated("Use ensureInitialized instead")
void initialize() {
  ensureInitialized();
}
