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

class MoqAudioDecoderOutput {
  final MoqAudioSampleFormat format;
  final int? sampleRate;
  final int? channels;
  final Duration? maxDelay;
  final Duration? delay;
  MoqAudioDecoderOutput({
    required this.format,
    this.sampleRate = null,
    this.channels = null,
    this.maxDelay = null,
    this.delay = null,
  });
}

class FfiConverterMoqAudioDecoderOutput {
  static MoqAudioDecoderOutput lift(RustBuffer buf) {
    return FfiConverterMoqAudioDecoderOutput.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioDecoderOutput> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final format_lifted = FfiConverterMoqAudioSampleFormat.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final format = format_lifted.value;
    new_offset += format_lifted.bytesRead;
    final sampleRate_lifted = FfiConverterOptionalUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final sampleRate = sampleRate_lifted.value;
    new_offset += sampleRate_lifted.bytesRead;
    final channels_lifted = FfiConverterOptionalUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final channels = channels_lifted.value;
    new_offset += channels_lifted.bytesRead;
    final maxDelay_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final maxDelay = maxDelay_lifted.value;
    new_offset += maxDelay_lifted.bytesRead;
    final delay_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final delay = delay_lifted.value;
    new_offset += delay_lifted.bytesRead;
    return LiftRetVal(
      MoqAudioDecoderOutput(
        format: format,
        sampleRate: sampleRate,
        channels: channels,
        maxDelay: maxDelay,
        delay: delay,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudioDecoderOutput value) {
    final total_length =
        FfiConverterMoqAudioSampleFormat.allocationSize(value.format) +
        FfiConverterOptionalUInt32.allocationSize(value.sampleRate) +
        FfiConverterOptionalUInt32.allocationSize(value.channels) +
        FfiConverterOptionalDuration.allocationSize(value.maxDelay) +
        FfiConverterOptionalDuration.allocationSize(value.delay) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudioDecoderOutput value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqAudioSampleFormat.write(
      value.format,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt32.write(
      value.sampleRate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt32.write(
      value.channels,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.maxDelay,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.delay,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudioDecoderOutput value) {
    return FfiConverterMoqAudioSampleFormat.allocationSize(value.format) +
        FfiConverterOptionalUInt32.allocationSize(value.sampleRate) +
        FfiConverterOptionalUInt32.allocationSize(value.channels) +
        FfiConverterOptionalDuration.allocationSize(value.maxDelay) +
        FfiConverterOptionalDuration.allocationSize(value.delay) +
        0;
  }
}

class MoqAudioEncoderFinished {
  final List<MoqAudioPacket> packets;
  final int discardPadding;
  MoqAudioEncoderFinished({
    required this.packets,
    required this.discardPadding,
  });
}

class FfiConverterMoqAudioEncoderFinished {
  static MoqAudioEncoderFinished lift(RustBuffer buf) {
    return FfiConverterMoqAudioEncoderFinished.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioEncoderFinished> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final packets_lifted = FfiConverterSequenceMoqAudioPacket.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final packets = packets_lifted.value;
    new_offset += packets_lifted.bytesRead;
    final discardPadding_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final discardPadding = discardPadding_lifted.value;
    new_offset += discardPadding_lifted.bytesRead;
    return LiftRetVal(
      MoqAudioEncoderFinished(packets: packets, discardPadding: discardPadding),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudioEncoderFinished value) {
    final total_length =
        FfiConverterSequenceMoqAudioPacket.allocationSize(value.packets) +
        FfiConverterUInt64.allocationSize(value.discardPadding) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudioEncoderFinished value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterSequenceMoqAudioPacket.write(
      value.packets,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.discardPadding,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudioEncoderFinished value) {
    return FfiConverterSequenceMoqAudioPacket.allocationSize(value.packets) +
        FfiConverterUInt64.allocationSize(value.discardPadding) +
        0;
  }
}

class MoqAudioEncoderInput {
  final MoqAudioSampleFormat format;
  final int sampleRate;
  final int channels;
  MoqAudioEncoderInput({
    required this.format,
    required this.sampleRate,
    required this.channels,
  });
}

class FfiConverterMoqAudioEncoderInput {
  static MoqAudioEncoderInput lift(RustBuffer buf) {
    return FfiConverterMoqAudioEncoderInput.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioEncoderInput> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final format_lifted = FfiConverterMoqAudioSampleFormat.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final format = format_lifted.value;
    new_offset += format_lifted.bytesRead;
    final sampleRate_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final sampleRate = sampleRate_lifted.value;
    new_offset += sampleRate_lifted.bytesRead;
    final channels_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final channels = channels_lifted.value;
    new_offset += channels_lifted.bytesRead;
    return LiftRetVal(
      MoqAudioEncoderInput(
        format: format,
        sampleRate: sampleRate,
        channels: channels,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudioEncoderInput value) {
    final total_length =
        FfiConverterMoqAudioSampleFormat.allocationSize(value.format) +
        FfiConverterUInt32.allocationSize(value.sampleRate) +
        FfiConverterUInt32.allocationSize(value.channels) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudioEncoderInput value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqAudioSampleFormat.write(
      value.format,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.sampleRate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.channels,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudioEncoderInput value) {
    return FfiConverterMoqAudioSampleFormat.allocationSize(value.format) +
        FfiConverterUInt32.allocationSize(value.sampleRate) +
        FfiConverterUInt32.allocationSize(value.channels) +
        0;
  }
}

class MoqAudioEncoderOutput {
  final MoqAudioCodec codec;
  final int? sampleRate;
  final int? channels;
  final int? bitrate;
  final Duration? frameDuration;
  MoqAudioEncoderOutput({
    required this.codec,
    this.sampleRate = null,
    this.channels = null,
    this.bitrate = null,
    this.frameDuration = null,
  });
}

class FfiConverterMoqAudioEncoderOutput {
  static MoqAudioEncoderOutput lift(RustBuffer buf) {
    return FfiConverterMoqAudioEncoderOutput.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioEncoderOutput> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final codec_lifted = FfiConverterMoqAudioCodec.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final codec = codec_lifted.value;
    new_offset += codec_lifted.bytesRead;
    final sampleRate_lifted = FfiConverterOptionalUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final sampleRate = sampleRate_lifted.value;
    new_offset += sampleRate_lifted.bytesRead;
    final channels_lifted = FfiConverterOptionalUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final channels = channels_lifted.value;
    new_offset += channels_lifted.bytesRead;
    final bitrate_lifted = FfiConverterOptionalUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bitrate = bitrate_lifted.value;
    new_offset += bitrate_lifted.bytesRead;
    final frameDuration_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final frameDuration = frameDuration_lifted.value;
    new_offset += frameDuration_lifted.bytesRead;
    return LiftRetVal(
      MoqAudioEncoderOutput(
        codec: codec,
        sampleRate: sampleRate,
        channels: channels,
        bitrate: bitrate,
        frameDuration: frameDuration,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudioEncoderOutput value) {
    final total_length =
        FfiConverterMoqAudioCodec.allocationSize(value.codec) +
        FfiConverterOptionalUInt32.allocationSize(value.sampleRate) +
        FfiConverterOptionalUInt32.allocationSize(value.channels) +
        FfiConverterOptionalUInt32.allocationSize(value.bitrate) +
        FfiConverterOptionalDuration.allocationSize(value.frameDuration) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudioEncoderOutput value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqAudioCodec.write(
      value.codec,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt32.write(
      value.sampleRate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt32.write(
      value.channels,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt32.write(
      value.bitrate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.frameDuration,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudioEncoderOutput value) {
    return FfiConverterMoqAudioCodec.allocationSize(value.codec) +
        FfiConverterOptionalUInt32.allocationSize(value.sampleRate) +
        FfiConverterOptionalUInt32.allocationSize(value.channels) +
        FfiConverterOptionalUInt32.allocationSize(value.bitrate) +
        FfiConverterOptionalDuration.allocationSize(value.frameDuration) +
        0;
  }
}

class MoqAudioFrame {
  final int timestampUs;
  final Uint8List data;
  MoqAudioFrame({required this.timestampUs, required this.data});
}

class FfiConverterMoqAudioFrame {
  static MoqAudioFrame lift(RustBuffer buf) {
    return FfiConverterMoqAudioFrame.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioFrame> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    final data_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final data = data_lifted.value;
    new_offset += data_lifted.bytesRead;
    return LiftRetVal(
      MoqAudioFrame(timestampUs: timestampUs, data: data),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudioFrame value) {
    final total_length =
        FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterUint8List.allocationSize(value.data) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudioFrame value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt64.write(
      value.timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUint8List.write(
      value.data,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudioFrame value) {
    return FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterUint8List.allocationSize(value.data) +
        0;
  }
}

class MoqAudioPacket {
  final Uint8List data;
  MoqAudioPacket({required this.data});
}

class FfiConverterMoqAudioPacket {
  static MoqAudioPacket lift(RustBuffer buf) {
    return FfiConverterMoqAudioPacket.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioPacket> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final data_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final data = data_lifted.value;
    new_offset += data_lifted.bytesRead;
    return LiftRetVal(
      MoqAudioPacket(data: data),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudioPacket value) {
    final total_length = FfiConverterUint8List.allocationSize(value.data) + 0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudioPacket value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUint8List.write(
      value.data,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudioPacket value) {
    return FfiConverterUint8List.allocationSize(value.data) + 0;
  }
}

class MoqAudioProducerOptions {
  final MoqAudioEncoderInput input;
  final MoqAudioEncoderOutput output;
  final String? track;
  final MoqAllocator? bandwidth;
  MoqAudioProducerOptions({
    required this.input,
    required this.output,
    this.track = null,
    this.bandwidth = null,
  });
}

class FfiConverterMoqAudioProducerOptions {
  static MoqAudioProducerOptions lift(RustBuffer buf) {
    return FfiConverterMoqAudioProducerOptions.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioProducerOptions> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final input_lifted = FfiConverterMoqAudioEncoderInput.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final input = input_lifted.value;
    new_offset += input_lifted.bytesRead;
    final output_lifted = FfiConverterMoqAudioEncoderOutput.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final output = output_lifted.value;
    new_offset += output_lifted.bytesRead;
    final track_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final track = track_lifted.value;
    new_offset += track_lifted.bytesRead;
    final bandwidth_lifted = FfiConverterOptionalMoqAllocator.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bandwidth = bandwidth_lifted.value;
    new_offset += bandwidth_lifted.bytesRead;
    return LiftRetVal(
      MoqAudioProducerOptions(
        input: input,
        output: output,
        track: track,
        bandwidth: bandwidth,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudioProducerOptions value) {
    final total_length =
        FfiConverterMoqAudioEncoderInput.allocationSize(value.input) +
        FfiConverterMoqAudioEncoderOutput.allocationSize(value.output) +
        FfiConverterOptionalString.allocationSize(value.track) +
        FfiConverterOptionalMoqAllocator.allocationSize(value.bandwidth) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudioProducerOptions value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqAudioEncoderInput.write(
      value.input,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqAudioEncoderOutput.write(
      value.output,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.track,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqAllocator.write(
      value.bandwidth,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudioProducerOptions value) {
    return FfiConverterMoqAudioEncoderInput.allocationSize(value.input) +
        FfiConverterMoqAudioEncoderOutput.allocationSize(value.output) +
        FfiConverterOptionalString.allocationSize(value.track) +
        FfiConverterOptionalMoqAllocator.allocationSize(value.bandwidth) +
        0;
  }
}

class MoqGrant {
  final List<String> publish;
  final List<String> subscribe;
  final Duration? expires;
  MoqGrant({required this.publish, required this.subscribe, this.expires});
}

class FfiConverterMoqGrant {
  static MoqGrant lift(RustBuffer buf) {
    return FfiConverterMoqGrant.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqGrant> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final publish_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final publish = publish_lifted.value;
    new_offset += publish_lifted.bytesRead;
    final subscribe_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final subscribe = subscribe_lifted.value;
    new_offset += subscribe_lifted.bytesRead;
    final expires_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final expires = expires_lifted.value;
    new_offset += expires_lifted.bytesRead;
    return LiftRetVal(
      MoqGrant(publish: publish, subscribe: subscribe, expires: expires),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqGrant value) {
    final total_length =
        FfiConverterSequenceString.allocationSize(value.publish) +
        FfiConverterSequenceString.allocationSize(value.subscribe) +
        FfiConverterOptionalDuration.allocationSize(value.expires) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqGrant value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterSequenceString.write(
      value.publish,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterSequenceString.write(
      value.subscribe,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.expires,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqGrant value) {
    return FfiConverterSequenceString.allocationSize(value.publish) +
        FfiConverterSequenceString.allocationSize(value.subscribe) +
        FfiConverterOptionalDuration.allocationSize(value.expires) +
        0;
  }
}

class MoqFetchGroupOptions {
  final int priority;
  final Duration? maxDelay;
  MoqFetchGroupOptions({this.priority = 0, this.maxDelay = null});
}

class FfiConverterMoqFetchGroupOptions {
  static MoqFetchGroupOptions lift(RustBuffer buf) {
    return FfiConverterMoqFetchGroupOptions.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqFetchGroupOptions> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final priority_lifted = FfiConverterInt8.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final priority = priority_lifted.value;
    new_offset += priority_lifted.bytesRead;
    final maxDelay_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final maxDelay = maxDelay_lifted.value;
    new_offset += maxDelay_lifted.bytesRead;
    return LiftRetVal(
      MoqFetchGroupOptions(priority: priority, maxDelay: maxDelay),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqFetchGroupOptions value) {
    final total_length =
        FfiConverterInt8.allocationSize(value.priority) +
        FfiConverterOptionalDuration.allocationSize(value.maxDelay) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqFetchGroupOptions value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterInt8.write(
      value.priority,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.maxDelay,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqFetchGroupOptions value) {
    return FfiConverterInt8.allocationSize(value.priority) +
        FfiConverterOptionalDuration.allocationSize(value.maxDelay) +
        0;
  }
}

class MoqRange {
  final MoqAnchor start;
  final MoqAnchor? end;
  MoqRange({required this.start, this.end = null});
}

class FfiConverterMoqRange {
  static MoqRange lift(RustBuffer buf) {
    return FfiConverterMoqRange.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqRange> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final start_lifted = FfiConverterMoqAnchor.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final start = start_lifted.value;
    new_offset += start_lifted.bytesRead;
    final end_lifted = FfiConverterOptionalMoqAnchor.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final end = end_lifted.value;
    new_offset += end_lifted.bytesRead;
    return LiftRetVal(
      MoqRange(start: start, end: end),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqRange value) {
    final total_length =
        FfiConverterMoqAnchor.allocationSize(value.start) +
        FfiConverterOptionalMoqAnchor.allocationSize(value.end) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqRange value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqAnchor.write(
      value.start,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqAnchor.write(
      value.end,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqRange value) {
    return FfiConverterMoqAnchor.allocationSize(value.start) +
        FfiConverterOptionalMoqAnchor.allocationSize(value.end) +
        0;
  }
}

class MoqSubscription {
  final int priority;
  final Duration? maxDelay;
  final List<MoqRange>? ranges;
  final MoqOrder? order;
  MoqSubscription({
    this.priority = 0,
    this.maxDelay,
    this.ranges = null,
    this.order = null,
  });
}

class FfiConverterMoqSubscription {
  static MoqSubscription lift(RustBuffer buf) {
    return FfiConverterMoqSubscription.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqSubscription> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final priority_lifted = FfiConverterInt8.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final priority = priority_lifted.value;
    new_offset += priority_lifted.bytesRead;
    final maxDelay_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final maxDelay = maxDelay_lifted.value;
    new_offset += maxDelay_lifted.bytesRead;
    final ranges_lifted = FfiConverterOptionalSequenceMoqRange.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final ranges = ranges_lifted.value;
    new_offset += ranges_lifted.bytesRead;
    final order_lifted = FfiConverterOptionalMoqOrder.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final order = order_lifted.value;
    new_offset += order_lifted.bytesRead;
    return LiftRetVal(
      MoqSubscription(
        priority: priority,
        maxDelay: maxDelay,
        ranges: ranges,
        order: order,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqSubscription value) {
    final total_length =
        FfiConverterInt8.allocationSize(value.priority) +
        FfiConverterOptionalDuration.allocationSize(value.maxDelay) +
        FfiConverterOptionalSequenceMoqRange.allocationSize(value.ranges) +
        FfiConverterOptionalMoqOrder.allocationSize(value.order) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqSubscription value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterInt8.write(
      value.priority,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.maxDelay,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalSequenceMoqRange.write(
      value.ranges,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqOrder.write(
      value.order,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqSubscription value) {
    return FfiConverterInt8.allocationSize(value.priority) +
        FfiConverterOptionalDuration.allocationSize(value.maxDelay) +
        FfiConverterOptionalSequenceMoqRange.allocationSize(value.ranges) +
        FfiConverterOptionalMoqOrder.allocationSize(value.order) +
        0;
  }
}

class MoqProtocolException {
  final MoqErrorScope scope;
  final int code;
  final MoqProtocolKind kind;
  final String message;
  MoqProtocolException({
    required this.scope,
    required this.code,
    required this.kind,
    required this.message,
  });
}

class FfiConverterMoqProtocolError {
  static MoqProtocolException lift(RustBuffer buf) {
    return FfiConverterMoqProtocolError.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqProtocolException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final scope_lifted = FfiConverterMoqErrorScope.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final scope = scope_lifted.value;
    new_offset += scope_lifted.bytesRead;
    final code_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final code = code_lifted.value;
    new_offset += code_lifted.bytesRead;
    final kind_lifted = FfiConverterMoqProtocolKind.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final kind = kind_lifted.value;
    new_offset += kind_lifted.bytesRead;
    final message_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final message = message_lifted.value;
    new_offset += message_lifted.bytesRead;
    return LiftRetVal(
      MoqProtocolException(
        scope: scope,
        code: code,
        kind: kind,
        message: message,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqProtocolException value) {
    final total_length =
        FfiConverterMoqErrorScope.allocationSize(value.scope) +
        FfiConverterUInt32.allocationSize(value.code) +
        FfiConverterMoqProtocolKind.allocationSize(value.kind) +
        FfiConverterString.allocationSize(value.message) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqProtocolException value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqErrorScope.write(
      value.scope,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.code,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqProtocolKind.write(
      value.kind,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterString.write(
      value.message,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqProtocolException value) {
    return FfiConverterMoqErrorScope.allocationSize(value.scope) +
        FfiConverterUInt32.allocationSize(value.code) +
        FfiConverterMoqProtocolKind.allocationSize(value.kind) +
        FfiConverterString.allocationSize(value.message) +
        0;
  }
}

class MoqJsonSnapshotConfig {
  final bool deltas;
  MoqJsonSnapshotConfig({this.deltas = true});
}

class FfiConverterMoqJsonSnapshotConfig {
  static MoqJsonSnapshotConfig lift(RustBuffer buf) {
    return FfiConverterMoqJsonSnapshotConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqJsonSnapshotConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final deltas_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final deltas = deltas_lifted.value;
    new_offset += deltas_lifted.bytesRead;
    return LiftRetVal(
      MoqJsonSnapshotConfig(deltas: deltas),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqJsonSnapshotConfig value) {
    final total_length = FfiConverterBool.allocationSize(value.deltas) + 0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqJsonSnapshotConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterBool.write(
      value.deltas,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqJsonSnapshotConfig value) {
    return FfiConverterBool.allocationSize(value.deltas) + 0;
  }
}

class MoqJsonValue {
  final String value;
  final int? timestampUs;
  MoqJsonValue({required this.value, this.timestampUs});
}

class FfiConverterMoqJsonValue {
  static MoqJsonValue lift(RustBuffer buf) {
    return FfiConverterMoqJsonValue.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqJsonValue> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final value_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final value = value_lifted.value;
    new_offset += value_lifted.bytesRead;
    final timestampUs_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    return LiftRetVal(
      MoqJsonValue(value: value, timestampUs: timestampUs),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqJsonValue value) {
    final total_length =
        FfiConverterString.allocationSize(value.value) +
        FfiConverterOptionalUInt64.allocationSize(value.timestampUs) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqJsonValue value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterString.write(
      value.value,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqJsonValue value) {
    return FfiConverterString.allocationSize(value.value) +
        FfiConverterOptionalUInt64.allocationSize(value.timestampUs) +
        0;
  }
}

class MoqJsonWindowConfig {
  final int? checkpointRecords;
  MoqJsonWindowConfig({this.checkpointRecords = null});
}

class FfiConverterMoqJsonWindowConfig {
  static MoqJsonWindowConfig lift(RustBuffer buf) {
    return FfiConverterMoqJsonWindowConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqJsonWindowConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final checkpointRecords_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final checkpointRecords = checkpointRecords_lifted.value;
    new_offset += checkpointRecords_lifted.bytesRead;
    return LiftRetVal(
      MoqJsonWindowConfig(checkpointRecords: checkpointRecords),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqJsonWindowConfig value) {
    final total_length =
        FfiConverterOptionalUInt64.allocationSize(value.checkpointRecords) + 0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqJsonWindowConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalUInt64.write(
      value.checkpointRecords,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqJsonWindowConfig value) {
    return FfiConverterOptionalUInt64.allocationSize(value.checkpointRecords) +
        0;
  }
}

class MoqJsonWindowTimed {
  final MoqJsonWindowEvent event;
  final int? timestampUs;
  MoqJsonWindowTimed({required this.event, this.timestampUs});
}

class FfiConverterMoqJsonWindowTimed {
  static MoqJsonWindowTimed lift(RustBuffer buf) {
    return FfiConverterMoqJsonWindowTimed.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqJsonWindowTimed> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final event_lifted = FfiConverterMoqJsonWindowEvent.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final event = event_lifted.value;
    new_offset += event_lifted.bytesRead;
    final timestampUs_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    return LiftRetVal(
      MoqJsonWindowTimed(event: event, timestampUs: timestampUs),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqJsonWindowTimed value) {
    final total_length =
        FfiConverterMoqJsonWindowEvent.allocationSize(value.event) +
        FfiConverterOptionalUInt64.allocationSize(value.timestampUs) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqJsonWindowTimed value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqJsonWindowEvent.write(
      value.event,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqJsonWindowTimed value) {
    return FfiConverterMoqJsonWindowEvent.allocationSize(value.event) +
        FfiConverterOptionalUInt64.allocationSize(value.timestampUs) +
        0;
  }
}

class MoqAudio {
  final String? label;
  final String? broadcast;
  final String? epoch;
  final String? track;
  final String codec;
  final Uint8List? description;
  final int sampleRate;
  final int channelCount;
  final int? bitrate;
  final bool enabled;
  final MoqContainer container;
  final Duration? jitter;
  MoqAudio({
    this.label = null,
    this.broadcast = null,
    this.epoch = null,
    this.track = null,
    required this.codec,
    this.description,
    required this.sampleRate,
    required this.channelCount,
    this.bitrate,
    this.enabled = true,
    required this.container,
    this.jitter = null,
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
    final epoch_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final epoch = epoch_lifted.value;
    new_offset += epoch_lifted.bytesRead;
    final track_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final track = track_lifted.value;
    new_offset += track_lifted.bytesRead;
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
    final enabled_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final enabled = enabled_lifted.value;
    new_offset += enabled_lifted.bytesRead;
    final container_lifted = FfiConverterMoqContainer.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final container = container_lifted.value;
    new_offset += container_lifted.bytesRead;
    final jitter_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final jitter = jitter_lifted.value;
    new_offset += jitter_lifted.bytesRead;
    return LiftRetVal(
      MoqAudio(
        label: label,
        broadcast: broadcast,
        epoch: epoch,
        track: track,
        codec: codec,
        description: description,
        sampleRate: sampleRate,
        channelCount: channelCount,
        bitrate: bitrate,
        enabled: enabled,
        container: container,
        jitter: jitter,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAudio value) {
    final total_length =
        FfiConverterOptionalString.allocationSize(value.label) +
        FfiConverterOptionalString.allocationSize(value.broadcast) +
        FfiConverterOptionalString.allocationSize(value.epoch) +
        FfiConverterOptionalString.allocationSize(value.track) +
        FfiConverterString.allocationSize(value.codec) +
        FfiConverterOptionalUint8List.allocationSize(value.description) +
        FfiConverterUInt32.allocationSize(value.sampleRate) +
        FfiConverterUInt32.allocationSize(value.channelCount) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterBool.allocationSize(value.enabled) +
        FfiConverterMoqContainer.allocationSize(value.container) +
        FfiConverterOptionalDuration.allocationSize(value.jitter) +
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
    new_offset += FfiConverterOptionalString.write(
      value.epoch,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.track,
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
    new_offset += FfiConverterBool.write(
      value.enabled,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqContainer.write(
      value.container,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.jitter,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAudio value) {
    return FfiConverterOptionalString.allocationSize(value.label) +
        FfiConverterOptionalString.allocationSize(value.broadcast) +
        FfiConverterOptionalString.allocationSize(value.epoch) +
        FfiConverterOptionalString.allocationSize(value.track) +
        FfiConverterString.allocationSize(value.codec) +
        FfiConverterOptionalUint8List.allocationSize(value.description) +
        FfiConverterUInt32.allocationSize(value.sampleRate) +
        FfiConverterUInt32.allocationSize(value.channelCount) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterBool.allocationSize(value.enabled) +
        FfiConverterMoqContainer.allocationSize(value.container) +
        FfiConverterOptionalDuration.allocationSize(value.jitter) +
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
  final MoqMediaTimebase? timebase;
  MoqContainerInit({
    required this.format,
    required this.data,
    this.timebase = null,
  });
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
    final timebase_lifted = FfiConverterOptionalMoqMediaTimebase.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timebase = timebase_lifted.value;
    new_offset += timebase_lifted.bytesRead;
    return LiftRetVal(
      MoqContainerInit(format: format, data: data, timebase: timebase),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqContainerInit value) {
    final total_length =
        FfiConverterMoqContainerFormat.allocationSize(value.format) +
        FfiConverterUint8List.allocationSize(value.data) +
        FfiConverterOptionalMoqMediaTimebase.allocationSize(value.timebase) +
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
    new_offset += FfiConverterOptionalMoqMediaTimebase.write(
      value.timebase,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqContainerInit value) {
    return FfiConverterMoqContainerFormat.allocationSize(value.format) +
        FfiConverterUint8List.allocationSize(value.data) +
        FfiConverterOptionalMoqMediaTimebase.allocationSize(value.timebase) +
        0;
  }
}

class MoqDatagram {
  final int sequence;
  final int? timestampUs;
  final Uint8List payload;
  MoqDatagram({
    this.sequence = 0,
    this.timestampUs = null,
    required this.payload,
  });
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
    final timestampUs_lifted = FfiConverterOptionalUInt64.read(
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
        FfiConverterOptionalUInt64.allocationSize(value.timestampUs) +
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
    new_offset += FfiConverterOptionalUInt64.write(
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
        FfiConverterOptionalUInt64.allocationSize(value.timestampUs) +
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
  final int? timestampUs;
  MoqFrame({required this.payload, this.timestampUs = null});
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
    final timestampUs_lifted = FfiConverterOptionalUInt64.read(
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
        FfiConverterOptionalUInt64.allocationSize(value.timestampUs) +
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
    new_offset += FfiConverterOptionalUInt64.write(
      value.timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqFrame value) {
    return FfiConverterUint8List.allocationSize(value.payload) +
        FfiConverterOptionalUInt64.allocationSize(value.timestampUs) +
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

class MoqMediaGroupConfig {
  final MoqRendition rendition;
  final int sequence;
  final MoqFetchGroupOptions? options;
  MoqMediaGroupConfig({
    required this.rendition,
    required this.sequence,
    this.options = null,
  });
}

class FfiConverterMoqMediaGroupConfig {
  static MoqMediaGroupConfig lift(RustBuffer buf) {
    return FfiConverterMoqMediaGroupConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqMediaGroupConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final rendition_lifted = FfiConverterMoqRendition.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final rendition = rendition_lifted.value;
    new_offset += rendition_lifted.bytesRead;
    final sequence_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final sequence = sequence_lifted.value;
    new_offset += sequence_lifted.bytesRead;
    final options_lifted = FfiConverterOptionalMoqFetchGroupOptions.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final options = options_lifted.value;
    new_offset += options_lifted.bytesRead;
    return LiftRetVal(
      MoqMediaGroupConfig(
        rendition: rendition,
        sequence: sequence,
        options: options,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqMediaGroupConfig value) {
    final total_length =
        FfiConverterMoqRendition.allocationSize(value.rendition) +
        FfiConverterUInt64.allocationSize(value.sequence) +
        FfiConverterOptionalMoqFetchGroupOptions.allocationSize(value.options) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqMediaGroupConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqRendition.write(
      value.rendition,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.sequence,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqFetchGroupOptions.write(
      value.options,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqMediaGroupConfig value) {
    return FfiConverterMoqRendition.allocationSize(value.rendition) +
        FfiConverterUInt64.allocationSize(value.sequence) +
        FfiConverterOptionalMoqFetchGroupOptions.allocationSize(value.options) +
        0;
  }
}

class MoqMediaTrackConfig {
  final MoqRendition rendition;
  final MoqSubscription? subscription;
  MoqMediaTrackConfig({required this.rendition, this.subscription = null});
}

class FfiConverterMoqMediaTrackConfig {
  static MoqMediaTrackConfig lift(RustBuffer buf) {
    return FfiConverterMoqMediaTrackConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqMediaTrackConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final rendition_lifted = FfiConverterMoqRendition.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final rendition = rendition_lifted.value;
    new_offset += rendition_lifted.bytesRead;
    final subscription_lifted = FfiConverterOptionalMoqSubscription.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final subscription = subscription_lifted.value;
    new_offset += subscription_lifted.bytesRead;
    return LiftRetVal(
      MoqMediaTrackConfig(rendition: rendition, subscription: subscription),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqMediaTrackConfig value) {
    final total_length =
        FfiConverterMoqRendition.allocationSize(value.rendition) +
        FfiConverterOptionalMoqSubscription.allocationSize(value.subscription) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqMediaTrackConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqRendition.write(
      value.rendition,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqSubscription.write(
      value.subscription,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqMediaTrackConfig value) {
    return FfiConverterMoqRendition.allocationSize(value.rendition) +
        FfiConverterOptionalMoqSubscription.allocationSize(value.subscription) +
        0;
  }
}

class MoqVideo {
  final String? label;
  final String? broadcast;
  final String? epoch;
  final String? track;
  final String codec;
  final Uint8List? description;
  final MoqDimensions? coded;
  final MoqDimensions? displayAspect;
  final int? bitrate;
  final bool enabled;
  final int preference;
  final double? framerate;
  final MoqContainer container;
  MoqVideo({
    this.label = null,
    this.broadcast = null,
    this.epoch = null,
    this.track = null,
    required this.codec,
    this.description,
    this.coded,
    this.displayAspect,
    this.bitrate,
    this.enabled = true,
    this.preference = 0,
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
    final epoch_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final epoch = epoch_lifted.value;
    new_offset += epoch_lifted.bytesRead;
    final track_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final track = track_lifted.value;
    new_offset += track_lifted.bytesRead;
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
    final enabled_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final enabled = enabled_lifted.value;
    new_offset += enabled_lifted.bytesRead;
    final preference_lifted = FfiConverterInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final preference = preference_lifted.value;
    new_offset += preference_lifted.bytesRead;
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
        epoch: epoch,
        track: track,
        codec: codec,
        description: description,
        coded: coded,
        displayAspect: displayAspect,
        bitrate: bitrate,
        enabled: enabled,
        preference: preference,
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
        FfiConverterOptionalString.allocationSize(value.epoch) +
        FfiConverterOptionalString.allocationSize(value.track) +
        FfiConverterString.allocationSize(value.codec) +
        FfiConverterOptionalUint8List.allocationSize(value.description) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.coded) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.displayAspect) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterBool.allocationSize(value.enabled) +
        FfiConverterInt32.allocationSize(value.preference) +
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
    new_offset += FfiConverterOptionalString.write(
      value.epoch,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.track,
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
      value.enabled,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterInt32.write(
      value.preference,
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
        FfiConverterOptionalString.allocationSize(value.epoch) +
        FfiConverterOptionalString.allocationSize(value.track) +
        FfiConverterString.allocationSize(value.codec) +
        FfiConverterOptionalUint8List.allocationSize(value.description) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.coded) +
        FfiConverterOptionalMoqDimensions.allocationSize(value.displayAspect) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterBool.allocationSize(value.enabled) +
        FfiConverterInt32.allocationSize(value.preference) +
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

class MoqAnnounce {
  final String prefix;
  final List<String>? captures;
  final MoqRoute route;
  MoqAnnounce({required this.prefix, this.captures, required this.route});
}

class FfiConverterMoqAnnounce {
  static MoqAnnounce lift(RustBuffer buf) {
    return FfiConverterMoqAnnounce.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAnnounce> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final prefix_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final prefix = prefix_lifted.value;
    new_offset += prefix_lifted.bytesRead;
    final captures_lifted = FfiConverterOptionalSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final captures = captures_lifted.value;
    new_offset += captures_lifted.bytesRead;
    final route_lifted = FfiConverterMoqRoute.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final route = route_lifted.value;
    new_offset += route_lifted.bytesRead;
    return LiftRetVal(
      MoqAnnounce(prefix: prefix, captures: captures, route: route),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAnnounce value) {
    final total_length =
        FfiConverterString.allocationSize(value.prefix) +
        FfiConverterOptionalSequenceString.allocationSize(value.captures) +
        FfiConverterMoqRoute.allocationSize(value.route) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAnnounce value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterString.write(
      value.prefix,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalSequenceString.write(
      value.captures,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqRoute.write(
      value.route,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAnnounce value) {
    return FfiConverterString.allocationSize(value.prefix) +
        FfiConverterOptionalSequenceString.allocationSize(value.captures) +
        FfiConverterMoqRoute.allocationSize(value.route) +
        0;
  }
}

class MoqAnnounceConfig {
  final String prefix;
  final String? filter;
  final bool hidden;
  MoqAnnounceConfig({
    this.prefix = '',
    this.filter = null,
    this.hidden = false,
  });
}

class FfiConverterMoqAnnounceConfig {
  static MoqAnnounceConfig lift(RustBuffer buf) {
    return FfiConverterMoqAnnounceConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAnnounceConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final prefix_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final prefix = prefix_lifted.value;
    new_offset += prefix_lifted.bytesRead;
    final filter_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final filter = filter_lifted.value;
    new_offset += filter_lifted.bytesRead;
    final hidden_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final hidden = hidden_lifted.value;
    new_offset += hidden_lifted.bytesRead;
    return LiftRetVal(
      MoqAnnounceConfig(prefix: prefix, filter: filter, hidden: hidden),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAnnounceConfig value) {
    final total_length =
        FfiConverterString.allocationSize(value.prefix) +
        FfiConverterOptionalString.allocationSize(value.filter) +
        FfiConverterBool.allocationSize(value.hidden) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAnnounceConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterString.write(
      value.prefix,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.filter,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterBool.write(
      value.hidden,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAnnounceConfig value) {
    return FfiConverterString.allocationSize(value.prefix) +
        FfiConverterOptionalString.allocationSize(value.filter) +
        FfiConverterBool.allocationSize(value.hidden) +
        0;
  }
}

class MoqAnnounceRoute {
  final int cost;
  final String? epoch;
  MoqAnnounceRoute({this.cost = 0, this.epoch = null});
}

class FfiConverterMoqAnnounceRoute {
  static MoqAnnounceRoute lift(RustBuffer buf) {
    return FfiConverterMoqAnnounceRoute.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAnnounceRoute> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final cost_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final cost = cost_lifted.value;
    new_offset += cost_lifted.bytesRead;
    final epoch_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final epoch = epoch_lifted.value;
    new_offset += epoch_lifted.bytesRead;
    return LiftRetVal(
      MoqAnnounceRoute(cost: cost, epoch: epoch),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqAnnounceRoute value) {
    final total_length =
        FfiConverterUInt64.allocationSize(value.cost) +
        FfiConverterOptionalString.allocationSize(value.epoch) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAnnounceRoute value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt64.write(
      value.cost,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.epoch,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqAnnounceRoute value) {
    return FfiConverterUInt64.allocationSize(value.cost) +
        FfiConverterOptionalString.allocationSize(value.epoch) +
        0;
  }
}

class MoqOriginConfig {
  final int? cacheCapacityBytes;
  MoqOriginConfig({this.cacheCapacityBytes = null});
}

class FfiConverterMoqOriginConfig {
  static MoqOriginConfig lift(RustBuffer buf) {
    return FfiConverterMoqOriginConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqOriginConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final cacheCapacityBytes_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final cacheCapacityBytes = cacheCapacityBytes_lifted.value;
    new_offset += cacheCapacityBytes_lifted.bytesRead;
    return LiftRetVal(
      MoqOriginConfig(cacheCapacityBytes: cacheCapacityBytes),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqOriginConfig value) {
    final total_length =
        FfiConverterOptionalUInt64.allocationSize(value.cacheCapacityBytes) + 0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqOriginConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalUInt64.write(
      value.cacheCapacityBytes,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqOriginConfig value) {
    return FfiConverterOptionalUInt64.allocationSize(value.cacheCapacityBytes) +
        0;
  }
}

class MoqRoute {
  final List<int> hops;
  final int cost;
  final bool anonymous;
  final String? epoch;
  MoqRoute({
    required this.hops,
    required this.cost,
    required this.anonymous,
    this.epoch,
  });
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
    final anonymous_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final anonymous = anonymous_lifted.value;
    new_offset += anonymous_lifted.bytesRead;
    final epoch_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final epoch = epoch_lifted.value;
    new_offset += epoch_lifted.bytesRead;
    return LiftRetVal(
      MoqRoute(hops: hops, cost: cost, anonymous: anonymous, epoch: epoch),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqRoute value) {
    final total_length =
        FfiConverterSequenceUInt64.allocationSize(value.hops) +
        FfiConverterUInt64.allocationSize(value.cost) +
        FfiConverterBool.allocationSize(value.anonymous) +
        FfiConverterOptionalString.allocationSize(value.epoch) +
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
    new_offset += FfiConverterBool.write(
      value.anonymous,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.epoch,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqRoute value) {
    return FfiConverterSequenceUInt64.allocationSize(value.hops) +
        FfiConverterUInt64.allocationSize(value.cost) +
        FfiConverterBool.allocationSize(value.anonymous) +
        FfiConverterOptionalString.allocationSize(value.epoch) +
        0;
  }
}

class MoqTrackInfo {
  final int priority;
  final Duration? maxAge;
  final int? refresh;
  final int? timescale;
  final bool compress;
  MoqTrackInfo({
    this.priority = 0,
    this.maxAge = null,
    this.refresh = null,
    this.timescale = null,
    this.compress = false,
  });
}

class FfiConverterMoqTrackInfo {
  static MoqTrackInfo lift(RustBuffer buf) {
    return FfiConverterMoqTrackInfo.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqTrackInfo> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final priority_lifted = FfiConverterInt8.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final priority = priority_lifted.value;
    new_offset += priority_lifted.bytesRead;
    final maxAge_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final maxAge = maxAge_lifted.value;
    new_offset += maxAge_lifted.bytesRead;
    final refresh_lifted = FfiConverterOptionalUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final refresh = refresh_lifted.value;
    new_offset += refresh_lifted.bytesRead;
    final timescale_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timescale = timescale_lifted.value;
    new_offset += timescale_lifted.bytesRead;
    final compress_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final compress = compress_lifted.value;
    new_offset += compress_lifted.bytesRead;
    return LiftRetVal(
      MoqTrackInfo(
        priority: priority,
        maxAge: maxAge,
        refresh: refresh,
        timescale: timescale,
        compress: compress,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqTrackInfo value) {
    final total_length =
        FfiConverterInt8.allocationSize(value.priority) +
        FfiConverterOptionalDuration.allocationSize(value.maxAge) +
        FfiConverterOptionalUInt32.allocationSize(value.refresh) +
        FfiConverterOptionalUInt64.allocationSize(value.timescale) +
        FfiConverterBool.allocationSize(value.compress) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqTrackInfo value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterInt8.write(
      value.priority,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.maxAge,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt32.write(
      value.refresh,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.timescale,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterBool.write(
      value.compress,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqTrackInfo value) {
    return FfiConverterInt8.allocationSize(value.priority) +
        FfiConverterOptionalDuration.allocationSize(value.maxAge) +
        FfiConverterOptionalUInt32.allocationSize(value.refresh) +
        FfiConverterOptionalUInt64.allocationSize(value.timescale) +
        FfiConverterBool.allocationSize(value.compress) +
        0;
  }
}

class MoqServerConfig {
  final String? bind;
  final List<String> versions;
  final MoqServerTls tls;
  final MoqQuicConfig quic;
  final String? implementation;
  final MoqOriginProducer? publish;
  final MoqOriginProducer? consume;
  MoqServerConfig({
    this.bind = null,
    this.versions = const [],
    required this.tls,
    required this.quic,
    this.implementation = null,
    this.publish = null,
    this.consume = null,
  });
}

class FfiConverterMoqServerConfig {
  static MoqServerConfig lift(RustBuffer buf) {
    return FfiConverterMoqServerConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqServerConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final bind_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bind = bind_lifted.value;
    new_offset += bind_lifted.bytesRead;
    final versions_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final versions = versions_lifted.value;
    new_offset += versions_lifted.bytesRead;
    final tls_lifted = FfiConverterMoqServerTls.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final tls = tls_lifted.value;
    new_offset += tls_lifted.bytesRead;
    final quic_lifted = FfiConverterMoqQuicConfig.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final quic = quic_lifted.value;
    new_offset += quic_lifted.bytesRead;
    final implementation_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final implementation = implementation_lifted.value;
    new_offset += implementation_lifted.bytesRead;
    final publish_lifted = FfiConverterOptionalMoqOriginProducer.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final publish = publish_lifted.value;
    new_offset += publish_lifted.bytesRead;
    final consume_lifted = FfiConverterOptionalMoqOriginProducer.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final consume = consume_lifted.value;
    new_offset += consume_lifted.bytesRead;
    return LiftRetVal(
      MoqServerConfig(
        bind: bind,
        versions: versions,
        tls: tls,
        quic: quic,
        implementation: implementation,
        publish: publish,
        consume: consume,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqServerConfig value) {
    final total_length =
        FfiConverterOptionalString.allocationSize(value.bind) +
        FfiConverterSequenceString.allocationSize(value.versions) +
        FfiConverterMoqServerTls.allocationSize(value.tls) +
        FfiConverterMoqQuicConfig.allocationSize(value.quic) +
        FfiConverterOptionalString.allocationSize(value.implementation) +
        FfiConverterOptionalMoqOriginProducer.allocationSize(value.publish) +
        FfiConverterOptionalMoqOriginProducer.allocationSize(value.consume) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqServerConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalString.write(
      value.bind,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterSequenceString.write(
      value.versions,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqServerTls.write(
      value.tls,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqQuicConfig.write(
      value.quic,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.implementation,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqOriginProducer.write(
      value.publish,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqOriginProducer.write(
      value.consume,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqServerConfig value) {
    return FfiConverterOptionalString.allocationSize(value.bind) +
        FfiConverterSequenceString.allocationSize(value.versions) +
        FfiConverterMoqServerTls.allocationSize(value.tls) +
        FfiConverterMoqQuicConfig.allocationSize(value.quic) +
        FfiConverterOptionalString.allocationSize(value.implementation) +
        FfiConverterOptionalMoqOriginProducer.allocationSize(value.publish) +
        FfiConverterOptionalMoqOriginProducer.allocationSize(value.consume) +
        0;
  }
}

class MoqServerTls {
  final List<String> cert;
  final List<String> key;
  final List<String> generate;
  MoqServerTls({
    this.cert = const [],
    this.key = const [],
    this.generate = const [],
  });
}

class FfiConverterMoqServerTls {
  static MoqServerTls lift(RustBuffer buf) {
    return FfiConverterMoqServerTls.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqServerTls> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final cert_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final cert = cert_lifted.value;
    new_offset += cert_lifted.bytesRead;
    final key_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final key = key_lifted.value;
    new_offset += key_lifted.bytesRead;
    final generate_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final generate = generate_lifted.value;
    new_offset += generate_lifted.bytesRead;
    return LiftRetVal(
      MoqServerTls(cert: cert, key: key, generate: generate),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqServerTls value) {
    final total_length =
        FfiConverterSequenceString.allocationSize(value.cert) +
        FfiConverterSequenceString.allocationSize(value.key) +
        FfiConverterSequenceString.allocationSize(value.generate) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqServerTls value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterSequenceString.write(
      value.cert,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterSequenceString.write(
      value.key,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterSequenceString.write(
      value.generate,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqServerTls value) {
    return FfiConverterSequenceString.allocationSize(value.cert) +
        FfiConverterSequenceString.allocationSize(value.key) +
        FfiConverterSequenceString.allocationSize(value.generate) +
        0;
  }
}

class MoqBackoff {
  final Duration? initial;
  final int? multiplier;
  final Duration? max;
  final Duration? timeout;
  MoqBackoff({
    this.initial = null,
    this.multiplier = null,
    this.max = null,
    this.timeout = null,
  });
}

class FfiConverterMoqBackoff {
  static MoqBackoff lift(RustBuffer buf) {
    return FfiConverterMoqBackoff.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqBackoff> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final initial_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final initial = initial_lifted.value;
    new_offset += initial_lifted.bytesRead;
    final multiplier_lifted = FfiConverterOptionalUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final multiplier = multiplier_lifted.value;
    new_offset += multiplier_lifted.bytesRead;
    final max_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final max = max_lifted.value;
    new_offset += max_lifted.bytesRead;
    final timeout_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timeout = timeout_lifted.value;
    new_offset += timeout_lifted.bytesRead;
    return LiftRetVal(
      MoqBackoff(
        initial: initial,
        multiplier: multiplier,
        max: max,
        timeout: timeout,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqBackoff value) {
    final total_length =
        FfiConverterOptionalDuration.allocationSize(value.initial) +
        FfiConverterOptionalUInt32.allocationSize(value.multiplier) +
        FfiConverterOptionalDuration.allocationSize(value.max) +
        FfiConverterOptionalDuration.allocationSize(value.timeout) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqBackoff value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalDuration.write(
      value.initial,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt32.write(
      value.multiplier,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.max,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.timeout,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqBackoff value) {
    return FfiConverterOptionalDuration.allocationSize(value.initial) +
        FfiConverterOptionalUInt32.allocationSize(value.multiplier) +
        FfiConverterOptionalDuration.allocationSize(value.max) +
        FfiConverterOptionalDuration.allocationSize(value.timeout) +
        0;
  }
}

class MoqClientConfig {
  final String? bind;
  final List<String> versions;
  final List<String> tokens;
  final Duration? timeout;
  final Duration? race;
  final Duration? resolutionDelay;
  final MoqClientTls tls;
  final MoqQuicConfig quic;
  final MoqWebSocketConfig websocket;
  final bool once;
  final MoqBackoff backoff;
  final String? implementation;
  final MoqOriginProducer? publish;
  final MoqOriginProducer? consume;
  MoqClientConfig({
    this.bind = null,
    this.versions = const [],
    this.tokens = const [],
    this.timeout = null,
    this.race = null,
    this.resolutionDelay = null,
    required this.tls,
    required this.quic,
    required this.websocket,
    this.once = false,
    required this.backoff,
    this.implementation = null,
    this.publish = null,
    this.consume = null,
  });
}

class FfiConverterMoqClientConfig {
  static MoqClientConfig lift(RustBuffer buf) {
    return FfiConverterMoqClientConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqClientConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final bind_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bind = bind_lifted.value;
    new_offset += bind_lifted.bytesRead;
    final versions_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final versions = versions_lifted.value;
    new_offset += versions_lifted.bytesRead;
    final tokens_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final tokens = tokens_lifted.value;
    new_offset += tokens_lifted.bytesRead;
    final timeout_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timeout = timeout_lifted.value;
    new_offset += timeout_lifted.bytesRead;
    final race_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final race = race_lifted.value;
    new_offset += race_lifted.bytesRead;
    final resolutionDelay_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final resolutionDelay = resolutionDelay_lifted.value;
    new_offset += resolutionDelay_lifted.bytesRead;
    final tls_lifted = FfiConverterMoqClientTls.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final tls = tls_lifted.value;
    new_offset += tls_lifted.bytesRead;
    final quic_lifted = FfiConverterMoqQuicConfig.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final quic = quic_lifted.value;
    new_offset += quic_lifted.bytesRead;
    final websocket_lifted = FfiConverterMoqWebSocketConfig.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final websocket = websocket_lifted.value;
    new_offset += websocket_lifted.bytesRead;
    final once_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final once = once_lifted.value;
    new_offset += once_lifted.bytesRead;
    final backoff_lifted = FfiConverterMoqBackoff.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final backoff = backoff_lifted.value;
    new_offset += backoff_lifted.bytesRead;
    final implementation_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final implementation = implementation_lifted.value;
    new_offset += implementation_lifted.bytesRead;
    final publish_lifted = FfiConverterOptionalMoqOriginProducer.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final publish = publish_lifted.value;
    new_offset += publish_lifted.bytesRead;
    final consume_lifted = FfiConverterOptionalMoqOriginProducer.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final consume = consume_lifted.value;
    new_offset += consume_lifted.bytesRead;
    return LiftRetVal(
      MoqClientConfig(
        bind: bind,
        versions: versions,
        tokens: tokens,
        timeout: timeout,
        race: race,
        resolutionDelay: resolutionDelay,
        tls: tls,
        quic: quic,
        websocket: websocket,
        once: once,
        backoff: backoff,
        implementation: implementation,
        publish: publish,
        consume: consume,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqClientConfig value) {
    final total_length =
        FfiConverterOptionalString.allocationSize(value.bind) +
        FfiConverterSequenceString.allocationSize(value.versions) +
        FfiConverterSequenceString.allocationSize(value.tokens) +
        FfiConverterOptionalDuration.allocationSize(value.timeout) +
        FfiConverterOptionalDuration.allocationSize(value.race) +
        FfiConverterOptionalDuration.allocationSize(value.resolutionDelay) +
        FfiConverterMoqClientTls.allocationSize(value.tls) +
        FfiConverterMoqQuicConfig.allocationSize(value.quic) +
        FfiConverterMoqWebSocketConfig.allocationSize(value.websocket) +
        FfiConverterBool.allocationSize(value.once) +
        FfiConverterMoqBackoff.allocationSize(value.backoff) +
        FfiConverterOptionalString.allocationSize(value.implementation) +
        FfiConverterOptionalMoqOriginProducer.allocationSize(value.publish) +
        FfiConverterOptionalMoqOriginProducer.allocationSize(value.consume) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqClientConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalString.write(
      value.bind,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterSequenceString.write(
      value.versions,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterSequenceString.write(
      value.tokens,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.timeout,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.race,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.resolutionDelay,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqClientTls.write(
      value.tls,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqQuicConfig.write(
      value.quic,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqWebSocketConfig.write(
      value.websocket,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterBool.write(
      value.once,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqBackoff.write(
      value.backoff,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.implementation,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqOriginProducer.write(
      value.publish,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqOriginProducer.write(
      value.consume,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqClientConfig value) {
    return FfiConverterOptionalString.allocationSize(value.bind) +
        FfiConverterSequenceString.allocationSize(value.versions) +
        FfiConverterSequenceString.allocationSize(value.tokens) +
        FfiConverterOptionalDuration.allocationSize(value.timeout) +
        FfiConverterOptionalDuration.allocationSize(value.race) +
        FfiConverterOptionalDuration.allocationSize(value.resolutionDelay) +
        FfiConverterMoqClientTls.allocationSize(value.tls) +
        FfiConverterMoqQuicConfig.allocationSize(value.quic) +
        FfiConverterMoqWebSocketConfig.allocationSize(value.websocket) +
        FfiConverterBool.allocationSize(value.once) +
        FfiConverterMoqBackoff.allocationSize(value.backoff) +
        FfiConverterOptionalString.allocationSize(value.implementation) +
        FfiConverterOptionalMoqOriginProducer.allocationSize(value.publish) +
        FfiConverterOptionalMoqOriginProducer.allocationSize(value.consume) +
        0;
  }
}

class MoqClientTls {
  final bool insecure;
  final List<String> roots;
  final bool? systemRoots;
  final List<String> fingerprints;
  final String? cert;
  final String? key;
  final String? hostName;
  MoqClientTls({
    this.insecure = false,
    this.roots = const [],
    this.systemRoots = null,
    this.fingerprints = const [],
    this.cert = null,
    this.key = null,
    this.hostName = null,
  });
}

class FfiConverterMoqClientTls {
  static MoqClientTls lift(RustBuffer buf) {
    return FfiConverterMoqClientTls.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqClientTls> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final insecure_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final insecure = insecure_lifted.value;
    new_offset += insecure_lifted.bytesRead;
    final roots_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final roots = roots_lifted.value;
    new_offset += roots_lifted.bytesRead;
    final systemRoots_lifted = FfiConverterOptionalBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final systemRoots = systemRoots_lifted.value;
    new_offset += systemRoots_lifted.bytesRead;
    final fingerprints_lifted = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final fingerprints = fingerprints_lifted.value;
    new_offset += fingerprints_lifted.bytesRead;
    final cert_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final cert = cert_lifted.value;
    new_offset += cert_lifted.bytesRead;
    final key_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final key = key_lifted.value;
    new_offset += key_lifted.bytesRead;
    final hostName_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final hostName = hostName_lifted.value;
    new_offset += hostName_lifted.bytesRead;
    return LiftRetVal(
      MoqClientTls(
        insecure: insecure,
        roots: roots,
        systemRoots: systemRoots,
        fingerprints: fingerprints,
        cert: cert,
        key: key,
        hostName: hostName,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqClientTls value) {
    final total_length =
        FfiConverterBool.allocationSize(value.insecure) +
        FfiConverterSequenceString.allocationSize(value.roots) +
        FfiConverterOptionalBool.allocationSize(value.systemRoots) +
        FfiConverterSequenceString.allocationSize(value.fingerprints) +
        FfiConverterOptionalString.allocationSize(value.cert) +
        FfiConverterOptionalString.allocationSize(value.key) +
        FfiConverterOptionalString.allocationSize(value.hostName) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqClientTls value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterBool.write(
      value.insecure,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterSequenceString.write(
      value.roots,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalBool.write(
      value.systemRoots,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterSequenceString.write(
      value.fingerprints,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.cert,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.key,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.hostName,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqClientTls value) {
    return FfiConverterBool.allocationSize(value.insecure) +
        FfiConverterSequenceString.allocationSize(value.roots) +
        FfiConverterOptionalBool.allocationSize(value.systemRoots) +
        FfiConverterSequenceString.allocationSize(value.fingerprints) +
        FfiConverterOptionalString.allocationSize(value.cert) +
        FfiConverterOptionalString.allocationSize(value.key) +
        FfiConverterOptionalString.allocationSize(value.hostName) +
        0;
  }
}

class MoqConnectionState {
  final int revision;
  final MoqConnectionPhase phase;
  final int connects;
  final MoqException? failure;
  final String? version;
  MoqConnectionState({
    required this.revision,
    required this.phase,
    required this.connects,
    this.failure,
    this.version,
  });
}

class FfiConverterMoqConnectionState {
  static MoqConnectionState lift(RustBuffer buf) {
    return FfiConverterMoqConnectionState.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqConnectionState> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final revision_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final revision = revision_lifted.value;
    new_offset += revision_lifted.bytesRead;
    final phase_lifted = FfiConverterMoqConnectionPhase.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final phase = phase_lifted.value;
    new_offset += phase_lifted.bytesRead;
    final connects_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final connects = connects_lifted.value;
    new_offset += connects_lifted.bytesRead;
    final failure_lifted = FfiConverterOptionalMoqException.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final failure = failure_lifted.value;
    new_offset += failure_lifted.bytesRead;
    final version_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final version = version_lifted.value;
    new_offset += version_lifted.bytesRead;
    return LiftRetVal(
      MoqConnectionState(
        revision: revision,
        phase: phase,
        connects: connects,
        failure: failure,
        version: version,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqConnectionState value) {
    final total_length =
        FfiConverterUInt64.allocationSize(value.revision) +
        FfiConverterMoqConnectionPhase.allocationSize(value.phase) +
        FfiConverterUInt64.allocationSize(value.connects) +
        FfiConverterOptionalMoqException.allocationSize(value.failure) +
        FfiConverterOptionalString.allocationSize(value.version) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqConnectionState value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt64.write(
      value.revision,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqConnectionPhase.write(
      value.phase,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      value.connects,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqException.write(
      value.failure,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.version,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqConnectionState value) {
    return FfiConverterUInt64.allocationSize(value.revision) +
        FfiConverterMoqConnectionPhase.allocationSize(value.phase) +
        FfiConverterUInt64.allocationSize(value.connects) +
        FfiConverterOptionalMoqException.allocationSize(value.failure) +
        FfiConverterOptionalString.allocationSize(value.version) +
        0;
  }
}

class MoqConnectionStats {
  final Duration? rtt;
  final int? estimatedSendRateBps;
  final int? bytesSent;
  final int? bytesReceived;
  final int? bytesLost;
  final int? packetsSent;
  final int? packetsReceived;
  final int? packetsLost;
  final String? version;
  MoqConnectionStats({
    this.rtt,
    this.estimatedSendRateBps,
    this.bytesSent,
    this.bytesReceived,
    this.bytesLost,
    this.packetsSent,
    this.packetsReceived,
    this.packetsLost,
    this.version = null,
  });
}

class FfiConverterMoqConnectionStats {
  static MoqConnectionStats lift(RustBuffer buf) {
    return FfiConverterMoqConnectionStats.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqConnectionStats> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final rtt_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final rtt = rtt_lifted.value;
    new_offset += rtt_lifted.bytesRead;
    final estimatedSendRateBps_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final estimatedSendRateBps = estimatedSendRateBps_lifted.value;
    new_offset += estimatedSendRateBps_lifted.bytesRead;
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
    final version_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final version = version_lifted.value;
    new_offset += version_lifted.bytesRead;
    return LiftRetVal(
      MoqConnectionStats(
        rtt: rtt,
        estimatedSendRateBps: estimatedSendRateBps,
        bytesSent: bytesSent,
        bytesReceived: bytesReceived,
        bytesLost: bytesLost,
        packetsSent: packetsSent,
        packetsReceived: packetsReceived,
        packetsLost: packetsLost,
        version: version,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqConnectionStats value) {
    final total_length =
        FfiConverterOptionalDuration.allocationSize(value.rtt) +
        FfiConverterOptionalUInt64.allocationSize(value.estimatedSendRateBps) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesSent) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesReceived) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesLost) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsSent) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsReceived) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsLost) +
        FfiConverterOptionalString.allocationSize(value.version) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqConnectionStats value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalDuration.write(
      value.rtt,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.estimatedSendRateBps,
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
    new_offset += FfiConverterOptionalString.write(
      value.version,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqConnectionStats value) {
    return FfiConverterOptionalDuration.allocationSize(value.rtt) +
        FfiConverterOptionalUInt64.allocationSize(value.estimatedSendRateBps) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesSent) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesReceived) +
        FfiConverterOptionalUInt64.allocationSize(value.bytesLost) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsSent) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsReceived) +
        FfiConverterOptionalUInt64.allocationSize(value.packetsLost) +
        FfiConverterOptionalString.allocationSize(value.version) +
        0;
  }
}

class MoqQuicConfig {
  final int? maxStreams;
  final Duration? idleTimeout;
  final Duration? keepAlive;
  final bool? gso;
  final bool? mtuDiscovery;
  final MoqCongestionControl? congestionControl;
  final String? qlog;
  final bool? keylog;
  MoqQuicConfig({
    this.maxStreams = null,
    this.idleTimeout = null,
    this.keepAlive = null,
    this.gso = null,
    this.mtuDiscovery = null,
    this.congestionControl = null,
    this.qlog = null,
    this.keylog = null,
  });
}

class FfiConverterMoqQuicConfig {
  static MoqQuicConfig lift(RustBuffer buf) {
    return FfiConverterMoqQuicConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqQuicConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final maxStreams_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final maxStreams = maxStreams_lifted.value;
    new_offset += maxStreams_lifted.bytesRead;
    final idleTimeout_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final idleTimeout = idleTimeout_lifted.value;
    new_offset += idleTimeout_lifted.bytesRead;
    final keepAlive_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final keepAlive = keepAlive_lifted.value;
    new_offset += keepAlive_lifted.bytesRead;
    final gso_lifted = FfiConverterOptionalBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final gso = gso_lifted.value;
    new_offset += gso_lifted.bytesRead;
    final mtuDiscovery_lifted = FfiConverterOptionalBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final mtuDiscovery = mtuDiscovery_lifted.value;
    new_offset += mtuDiscovery_lifted.bytesRead;
    final congestionControl_lifted =
        FfiConverterOptionalMoqCongestionControl.read(
          Uint8List.view(buf.buffer, new_offset),
        );
    final congestionControl = congestionControl_lifted.value;
    new_offset += congestionControl_lifted.bytesRead;
    final qlog_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final qlog = qlog_lifted.value;
    new_offset += qlog_lifted.bytesRead;
    final keylog_lifted = FfiConverterOptionalBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final keylog = keylog_lifted.value;
    new_offset += keylog_lifted.bytesRead;
    return LiftRetVal(
      MoqQuicConfig(
        maxStreams: maxStreams,
        idleTimeout: idleTimeout,
        keepAlive: keepAlive,
        gso: gso,
        mtuDiscovery: mtuDiscovery,
        congestionControl: congestionControl,
        qlog: qlog,
        keylog: keylog,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqQuicConfig value) {
    final total_length =
        FfiConverterOptionalUInt64.allocationSize(value.maxStreams) +
        FfiConverterOptionalDuration.allocationSize(value.idleTimeout) +
        FfiConverterOptionalDuration.allocationSize(value.keepAlive) +
        FfiConverterOptionalBool.allocationSize(value.gso) +
        FfiConverterOptionalBool.allocationSize(value.mtuDiscovery) +
        FfiConverterOptionalMoqCongestionControl.allocationSize(
          value.congestionControl,
        ) +
        FfiConverterOptionalString.allocationSize(value.qlog) +
        FfiConverterOptionalBool.allocationSize(value.keylog) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqQuicConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalUInt64.write(
      value.maxStreams,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.idleTimeout,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.keepAlive,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalBool.write(
      value.gso,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalBool.write(
      value.mtuDiscovery,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqCongestionControl.write(
      value.congestionControl,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.qlog,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalBool.write(
      value.keylog,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqQuicConfig value) {
    return FfiConverterOptionalUInt64.allocationSize(value.maxStreams) +
        FfiConverterOptionalDuration.allocationSize(value.idleTimeout) +
        FfiConverterOptionalDuration.allocationSize(value.keepAlive) +
        FfiConverterOptionalBool.allocationSize(value.gso) +
        FfiConverterOptionalBool.allocationSize(value.mtuDiscovery) +
        FfiConverterOptionalMoqCongestionControl.allocationSize(
          value.congestionControl,
        ) +
        FfiConverterOptionalString.allocationSize(value.qlog) +
        FfiConverterOptionalBool.allocationSize(value.keylog) +
        0;
  }
}

class MoqWebSocketConfig {
  final bool? enabled;
  final Duration? delay;
  MoqWebSocketConfig({this.enabled = null, this.delay = null});
}

class FfiConverterMoqWebSocketConfig {
  static MoqWebSocketConfig lift(RustBuffer buf) {
    return FfiConverterMoqWebSocketConfig.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqWebSocketConfig> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final enabled_lifted = FfiConverterOptionalBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final enabled = enabled_lifted.value;
    new_offset += enabled_lifted.bytesRead;
    final delay_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final delay = delay_lifted.value;
    new_offset += delay_lifted.bytesRead;
    return LiftRetVal(
      MoqWebSocketConfig(enabled: enabled, delay: delay),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqWebSocketConfig value) {
    final total_length =
        FfiConverterOptionalBool.allocationSize(value.enabled) +
        FfiConverterOptionalDuration.allocationSize(value.delay) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqWebSocketConfig value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalBool.write(
      value.enabled,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.delay,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqWebSocketConfig value) {
    return FfiConverterOptionalBool.allocationSize(value.enabled) +
        FfiConverterOptionalDuration.allocationSize(value.delay) +
        0;
  }
}

class MoqVideoDecoderOutput {
  final MoqDimensions? resize;
  final Duration? maxDelay;
  final bool surface;
  MoqVideoDecoderOutput({
    this.resize = null,
    this.maxDelay = null,
    this.surface = false,
  });
}

class FfiConverterMoqVideoDecoderOutput {
  static MoqVideoDecoderOutput lift(RustBuffer buf) {
    return FfiConverterMoqVideoDecoderOutput.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoDecoderOutput> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final resize_lifted = FfiConverterOptionalMoqDimensions.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final resize = resize_lifted.value;
    new_offset += resize_lifted.bytesRead;
    final maxDelay_lifted = FfiConverterOptionalDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final maxDelay = maxDelay_lifted.value;
    new_offset += maxDelay_lifted.bytesRead;
    final surface_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final surface = surface_lifted.value;
    new_offset += surface_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoDecoderOutput(
        resize: resize,
        maxDelay: maxDelay,
        surface: surface,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoDecoderOutput value) {
    final total_length =
        FfiConverterOptionalMoqDimensions.allocationSize(value.resize) +
        FfiConverterOptionalDuration.allocationSize(value.maxDelay) +
        FfiConverterBool.allocationSize(value.surface) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoDecoderOutput value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterOptionalMoqDimensions.write(
      value.resize,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalDuration.write(
      value.maxDelay,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterBool.write(
      value.surface,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoDecoderOutput value) {
    return FfiConverterOptionalMoqDimensions.allocationSize(value.resize) +
        FfiConverterOptionalDuration.allocationSize(value.maxDelay) +
        FfiConverterBool.allocationSize(value.surface) +
        0;
  }
}

class MoqVideoEncoderInput {
  final MoqVideoPixelFormat format;
  final int width;
  final int height;
  final MoqVideoFrameRate framerate;
  MoqVideoEncoderInput({
    required this.format,
    required this.width,
    required this.height,
    required this.framerate,
  });
}

class FfiConverterMoqVideoEncoderInput {
  static MoqVideoEncoderInput lift(RustBuffer buf) {
    return FfiConverterMoqVideoEncoderInput.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoEncoderInput> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final format_lifted = FfiConverterMoqVideoPixelFormat.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final format = format_lifted.value;
    new_offset += format_lifted.bytesRead;
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
    final framerate_lifted = FfiConverterMoqVideoFrameRate.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final framerate = framerate_lifted.value;
    new_offset += framerate_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoEncoderInput(
        format: format,
        width: width,
        height: height,
        framerate: framerate,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoEncoderInput value) {
    final total_length =
        FfiConverterMoqVideoPixelFormat.allocationSize(value.format) +
        FfiConverterUInt32.allocationSize(value.width) +
        FfiConverterUInt32.allocationSize(value.height) +
        FfiConverterMoqVideoFrameRate.allocationSize(value.framerate) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoEncoderInput value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqVideoPixelFormat.write(
      value.format,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.width,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.height,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqVideoFrameRate.write(
      value.framerate,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoEncoderInput value) {
    return FfiConverterMoqVideoPixelFormat.allocationSize(value.format) +
        FfiConverterUInt32.allocationSize(value.width) +
        FfiConverterUInt32.allocationSize(value.height) +
        FfiConverterMoqVideoFrameRate.allocationSize(value.framerate) +
        0;
  }
}

class MoqVideoEncoderOutput {
  final MoqVideoCodec codec;
  final int? bitrate;
  final MoqVideoGop? gop;
  final MoqVideoEncoderKind kind;
  MoqVideoEncoderOutput({
    required this.codec,
    this.bitrate = null,
    this.gop = null,
    required this.kind,
  });
}

class FfiConverterMoqVideoEncoderOutput {
  static MoqVideoEncoderOutput lift(RustBuffer buf) {
    return FfiConverterMoqVideoEncoderOutput.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoEncoderOutput> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final codec_lifted = FfiConverterMoqVideoCodec.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final codec = codec_lifted.value;
    new_offset += codec_lifted.bytesRead;
    final bitrate_lifted = FfiConverterOptionalUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bitrate = bitrate_lifted.value;
    new_offset += bitrate_lifted.bytesRead;
    final gop_lifted = FfiConverterOptionalMoqVideoGop.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final gop = gop_lifted.value;
    new_offset += gop_lifted.bytesRead;
    final kind_lifted = FfiConverterMoqVideoEncoderKind.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final kind = kind_lifted.value;
    new_offset += kind_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoEncoderOutput(
        codec: codec,
        bitrate: bitrate,
        gop: gop,
        kind: kind,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoEncoderOutput value) {
    final total_length =
        FfiConverterMoqVideoCodec.allocationSize(value.codec) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterOptionalMoqVideoGop.allocationSize(value.gop) +
        FfiConverterMoqVideoEncoderKind.allocationSize(value.kind) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoEncoderOutput value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqVideoCodec.write(
      value.codec,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalUInt64.write(
      value.bitrate,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqVideoGop.write(
      value.gop,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqVideoEncoderKind.write(
      value.kind,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoEncoderOutput value) {
    return FfiConverterMoqVideoCodec.allocationSize(value.codec) +
        FfiConverterOptionalUInt64.allocationSize(value.bitrate) +
        FfiConverterOptionalMoqVideoGop.allocationSize(value.gop) +
        FfiConverterMoqVideoEncoderKind.allocationSize(value.kind) +
        0;
  }
}

class MoqVideoFrame {
  final int timestampUs;
  final Uint8List data;
  MoqVideoFrame({required this.timestampUs, required this.data});
}

class FfiConverterMoqVideoFrame {
  static MoqVideoFrame lift(RustBuffer buf) {
    return FfiConverterMoqVideoFrame.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoFrame> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    final data_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final data = data_lifted.value;
    new_offset += data_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoFrame(timestampUs: timestampUs, data: data),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoFrame value) {
    final total_length =
        FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterUint8List.allocationSize(value.data) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoFrame value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt64.write(
      value.timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUint8List.write(
      value.data,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoFrame value) {
    return FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterUint8List.allocationSize(value.data) +
        0;
  }
}

class MoqVideoFrameRate {
  final int numerator;
  final int denominator;
  MoqVideoFrameRate({required this.numerator, required this.denominator});
}

class FfiConverterMoqVideoFrameRate {
  static MoqVideoFrameRate lift(RustBuffer buf) {
    return FfiConverterMoqVideoFrameRate.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoFrameRate> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final numerator_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final numerator = numerator_lifted.value;
    new_offset += numerator_lifted.bytesRead;
    final denominator_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final denominator = denominator_lifted.value;
    new_offset += denominator_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoFrameRate(numerator: numerator, denominator: denominator),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoFrameRate value) {
    final total_length =
        FfiConverterUInt32.allocationSize(value.numerator) +
        FfiConverterUInt32.allocationSize(value.denominator) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoFrameRate value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt32.write(
      value.numerator,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt32.write(
      value.denominator,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoFrameRate value) {
    return FfiConverterUInt32.allocationSize(value.numerator) +
        FfiConverterUInt32.allocationSize(value.denominator) +
        0;
  }
}

class MoqVideoPacket {
  final int timestampUs;
  final Uint8List data;
  final bool keyframe;
  MoqVideoPacket({
    required this.timestampUs,
    required this.data,
    required this.keyframe,
  });
}

class FfiConverterMoqVideoPacket {
  static MoqVideoPacket lift(RustBuffer buf) {
    return FfiConverterMoqVideoPacket.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoPacket> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    final data_lifted = FfiConverterUint8List.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final data = data_lifted.value;
    new_offset += data_lifted.bytesRead;
    final keyframe_lifted = FfiConverterBool.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final keyframe = keyframe_lifted.value;
    new_offset += keyframe_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoPacket(timestampUs: timestampUs, data: data, keyframe: keyframe),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoPacket value) {
    final total_length =
        FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterUint8List.allocationSize(value.data) +
        FfiConverterBool.allocationSize(value.keyframe) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoPacket value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterUInt64.write(
      value.timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUint8List.write(
      value.data,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterBool.write(
      value.keyframe,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoPacket value) {
    return FfiConverterUInt64.allocationSize(value.timestampUs) +
        FfiConverterUint8List.allocationSize(value.data) +
        FfiConverterBool.allocationSize(value.keyframe) +
        0;
  }
}

class MoqVideoProducerOptions {
  final MoqVideoEncoderInput input;
  final MoqVideoEncoderOutput output;
  final String? track;
  final MoqAllocator? bandwidth;
  final MoqVideoProducer? lower;
  MoqVideoProducerOptions({
    required this.input,
    required this.output,
    this.track = null,
    this.bandwidth = null,
    this.lower = null,
  });
}

class FfiConverterMoqVideoProducerOptions {
  static MoqVideoProducerOptions lift(RustBuffer buf) {
    return FfiConverterMoqVideoProducerOptions.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoProducerOptions> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final input_lifted = FfiConverterMoqVideoEncoderInput.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final input = input_lifted.value;
    new_offset += input_lifted.bytesRead;
    final output_lifted = FfiConverterMoqVideoEncoderOutput.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final output = output_lifted.value;
    new_offset += output_lifted.bytesRead;
    final track_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final track = track_lifted.value;
    new_offset += track_lifted.bytesRead;
    final bandwidth_lifted = FfiConverterOptionalMoqAllocator.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final bandwidth = bandwidth_lifted.value;
    new_offset += bandwidth_lifted.bytesRead;
    final lower_lifted = FfiConverterOptionalMoqVideoProducer.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final lower = lower_lifted.value;
    new_offset += lower_lifted.bytesRead;
    return LiftRetVal(
      MoqVideoProducerOptions(
        input: input,
        output: output,
        track: track,
        bandwidth: bandwidth,
        lower: lower,
      ),
      new_offset - buf.offsetInBytes,
    );
  }

  static RustBuffer lower(MoqVideoProducerOptions value) {
    final total_length =
        FfiConverterMoqVideoEncoderInput.allocationSize(value.input) +
        FfiConverterMoqVideoEncoderOutput.allocationSize(value.output) +
        FfiConverterOptionalString.allocationSize(value.track) +
        FfiConverterOptionalMoqAllocator.allocationSize(value.bandwidth) +
        FfiConverterOptionalMoqVideoProducer.allocationSize(value.lower) +
        0;
    final buf = Uint8List(total_length);
    write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoProducerOptions value, Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    new_offset += FfiConverterMoqVideoEncoderInput.write(
      value.input,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqVideoEncoderOutput.write(
      value.output,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalString.write(
      value.track,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqAllocator.write(
      value.bandwidth,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterOptionalMoqVideoProducer.write(
      value.lower,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset - buf.offsetInBytes;
  }

  static int allocationSize(MoqVideoProducerOptions value) {
    return FfiConverterMoqVideoEncoderInput.allocationSize(value.input) +
        FfiConverterMoqVideoEncoderOutput.allocationSize(value.output) +
        FfiConverterOptionalString.allocationSize(value.track) +
        FfiConverterOptionalMoqAllocator.allocationSize(value.bandwidth) +
        FfiConverterOptionalMoqVideoProducer.allocationSize(value.lower) +
        0;
  }
}

abstract class MoqAudioRead {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqAudioRead {
  static MoqAudioRead lift(RustBuffer buffer) {
    return FfiConverterMoqAudioRead.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioRead> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = FrameMoqAudioRead.read(subview);
        return LiftRetVal<MoqAudioRead>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = GapMoqAudioRead.read(subview);
        return LiftRetVal<MoqAudioRead>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 3:
        final lifted = EndMoqAudioRead.read(subview);
        return LiftRetVal<MoqAudioRead>(
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

  static RustBuffer lower(MoqAudioRead value) {
    return value.lower();
  }

  static int allocationSize(MoqAudioRead value) {
    return value.allocationSize();
  }

  static int write(MoqAudioRead value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class FrameMoqAudioRead extends MoqAudioRead {
  final MoqAudioFrame frame;
  FrameMoqAudioRead(MoqAudioFrame this.frame);
  FrameMoqAudioRead._(MoqAudioFrame this.frame);
  static LiftRetVal<FrameMoqAudioRead> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final frame_lifted = FfiConverterMoqAudioFrame.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final frame = frame_lifted.value;
    new_offset += frame_lifted.bytesRead;
    return LiftRetVal(FrameMoqAudioRead._(frame), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterMoqAudioFrame.allocationSize(frame) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterMoqAudioFrame.write(
      frame,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class GapMoqAudioRead extends MoqAudioRead {
  final int timestampUs;
  final Duration duration;
  GapMoqAudioRead({
    required int this.timestampUs,
    required Duration this.duration,
  });
  GapMoqAudioRead._(int this.timestampUs, Duration this.duration);
  static LiftRetVal<GapMoqAudioRead> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    final duration_lifted = FfiConverterDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final duration = duration_lifted.value;
    new_offset += duration_lifted.bytesRead;
    return LiftRetVal(GapMoqAudioRead._(timestampUs, duration), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(timestampUs) +
        FfiConverterDuration.allocationSize(duration) +
        4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterDuration.write(
      duration,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class EndMoqAudioRead extends MoqAudioRead {
  final int endUs;
  EndMoqAudioRead(int this.endUs);
  EndMoqAudioRead._(int this.endUs);
  static LiftRetVal<EndMoqAudioRead> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final endUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final endUs = endUs_lifted.value;
    new_offset += endUs_lifted.bytesRead;
    return LiftRetVal(EndMoqAudioRead._(endUs), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(endUs) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 3);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      endUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

enum MoqAudioSampleFormat {
  u8,
  s16,
  s32,
  f32,
  u8Planar,
  s16Planar,
  s32Planar,
  f32Planar,
}

class FfiConverterMoqAudioSampleFormat {
  static LiftRetVal<MoqAudioSampleFormat> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqAudioSampleFormat.u8, 4);
      case 2:
        return LiftRetVal(MoqAudioSampleFormat.s16, 4);
      case 3:
        return LiftRetVal(MoqAudioSampleFormat.s32, 4);
      case 4:
        return LiftRetVal(MoqAudioSampleFormat.f32, 4);
      case 5:
        return LiftRetVal(MoqAudioSampleFormat.u8Planar, 4);
      case 6:
        return LiftRetVal(MoqAudioSampleFormat.s16Planar, 4);
      case 7:
        return LiftRetVal(MoqAudioSampleFormat.s32Planar, 4);
      case 8:
        return LiftRetVal(MoqAudioSampleFormat.f32Planar, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqAudioSampleFormat lift(RustBuffer buffer) {
    return FfiConverterMoqAudioSampleFormat.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqAudioSampleFormat input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqAudioSampleFormat _value) {
    return 4;
  }

  static int write(MoqAudioSampleFormat value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

abstract class MoqAnchor {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqAnchor {
  static MoqAnchor lift(RustBuffer buffer) {
    return FfiConverterMoqAnchor.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqAnchor> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = AbsoluteMoqAnchor.read(subview);
        return LiftRetVal<MoqAnchor>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = RelativeMoqAnchor.read(subview);
        return LiftRetVal<MoqAnchor>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 3:
        final lifted = TimeMoqAnchor.read(subview);
        return LiftRetVal<MoqAnchor>(
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

  static RustBuffer lower(MoqAnchor value) {
    return value.lower();
  }

  static int allocationSize(MoqAnchor value) {
    return value.allocationSize();
  }

  static int write(MoqAnchor value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class AbsoluteMoqAnchor extends MoqAnchor {
  final int group;
  AbsoluteMoqAnchor(int this.group);
  AbsoluteMoqAnchor._(int this.group);
  static LiftRetVal<AbsoluteMoqAnchor> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final group_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final group = group_lifted.value;
    new_offset += group_lifted.bytesRead;
    return LiftRetVal(AbsoluteMoqAnchor._(group), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(group) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      group,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class RelativeMoqAnchor extends MoqAnchor {
  final int groups;
  RelativeMoqAnchor(int this.groups);
  RelativeMoqAnchor._(int this.groups);
  static LiftRetVal<RelativeMoqAnchor> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final groups_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final groups = groups_lifted.value;
    new_offset += groups_lifted.bytesRead;
    return LiftRetVal(RelativeMoqAnchor._(groups), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(groups) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      groups,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class TimeMoqAnchor extends MoqAnchor {
  final int timestampUs;
  TimeMoqAnchor(int this.timestampUs);
  TimeMoqAnchor._(int this.timestampUs);
  static LiftRetVal<TimeMoqAnchor> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    return LiftRetVal(TimeMoqAnchor._(timestampUs), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(timestampUs) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 3);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

enum MoqOrder { ascending, descending }

class FfiConverterMoqOrder {
  static LiftRetVal<MoqOrder> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqOrder.ascending, 4);
      case 2:
        return LiftRetVal(MoqOrder.descending, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqOrder lift(RustBuffer buffer) {
    return FfiConverterMoqOrder.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqOrder input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqOrder _value) {
    return 4;
  }

  static int write(MoqOrder value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

abstract class MoqException implements Exception {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
  @override
  String toString() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqerror_uniffi_trait_display(
        FfiConverterMoqException.lower(this),
        status,
      ),
      FfiConverterString.lift,
    );
  }
}

class FfiConverterMoqException {
  static MoqException lift(RustBuffer buffer) {
    return FfiConverterMoqException.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqException> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = ProtocolMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = TransportMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 3:
        final lifted = InternalMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 4:
        final lifted = MediaMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 5:
        final lifted = MuxMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 6:
        final lifted = JsonTrackMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 7:
        final lifted = AudioMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 8:
        final lifted = VideoMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 9:
        final lifted = UrlMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 10:
        final lifted = TimeOverflowMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 11:
        final lifted = LogLevelMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 12:
        final lifted = TaskMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 13:
        final lifted = JsonMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 14:
        final lifted = CancelledMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 15:
        final lifted = ClosedMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 16:
        final lifted = BusyMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 17:
        final lifted = ConnectMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 18:
        final lifted = BindMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 19:
        final lifted = RejectMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 20:
        final lifted = AlreadyRespondedMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 21:
        final lifted = CodecMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 22:
        final lifted = UnauthorizedMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 23:
        final lifted = ForbiddenMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 24:
        final lifted = ExpiredMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 25:
        final lifted = NotFoundMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 26:
        final lifted = UnsupportedMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 27:
        final lifted = AlreadyCommittedMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 28:
        final lifted = InvalidPatternMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 29:
        final lifted = InvalidEpochMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 30:
        final lifted = UnresolvableBroadcastMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 31:
        final lifted = LogMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 32:
        final lifted = ConfigMoqException.read(subview);
        return LiftRetVal<MoqException>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 33:
        final lifted = UnsupportedCatalogMoqException.read(subview);
        return LiftRetVal<MoqException>(
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

  static RustBuffer lower(MoqException value) {
    return value.lower();
  }

  static int allocationSize(MoqException value) {
    return value.allocationSize();
  }

  static int write(MoqException value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class ProtocolMoqException extends MoqException {
  final MoqProtocolException details;
  ProtocolMoqException(MoqProtocolException this.details);
  ProtocolMoqException._(MoqProtocolException this.details);
  static LiftRetVal<ProtocolMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final details_lifted = FfiConverterMoqProtocolError.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final details = details_lifted.value;
    new_offset += details_lifted.bytesRead;
    return LiftRetVal(ProtocolMoqException._(details), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterMoqProtocolError.allocationSize(details) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterMoqProtocolError.write(
      details,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class TransportMoqException extends MoqException {
  final String reason;
  TransportMoqException(String this.reason);
  TransportMoqException._(String this.reason);
  static LiftRetVal<TransportMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(TransportMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class InternalMoqException extends MoqException {
  final String reason;
  InternalMoqException(String this.reason);
  InternalMoqException._(String this.reason);
  static LiftRetVal<InternalMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(InternalMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 3);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class MediaMoqException extends MoqException {
  final String reason;
  MediaMoqException(String this.reason);
  MediaMoqException._(String this.reason);
  static LiftRetVal<MediaMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(MediaMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 4);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class MuxMoqException extends MoqException {
  final String reason;
  MuxMoqException(String this.reason);
  MuxMoqException._(String this.reason);
  static LiftRetVal<MuxMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(MuxMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 5);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class JsonTrackMoqException extends MoqException {
  final String reason;
  JsonTrackMoqException(String this.reason);
  JsonTrackMoqException._(String this.reason);
  static LiftRetVal<JsonTrackMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(JsonTrackMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 6);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class AudioMoqException extends MoqException {
  final String reason;
  AudioMoqException(String this.reason);
  AudioMoqException._(String this.reason);
  static LiftRetVal<AudioMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(AudioMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 7);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class VideoMoqException extends MoqException {
  final String reason;
  VideoMoqException(String this.reason);
  VideoMoqException._(String this.reason);
  static LiftRetVal<VideoMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(VideoMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 8);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class UrlMoqException extends MoqException {
  final String reason;
  UrlMoqException(String this.reason);
  UrlMoqException._(String this.reason);
  static LiftRetVal<UrlMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(UrlMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 9);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class TimeOverflowMoqException extends MoqException {
  TimeOverflowMoqException();
  TimeOverflowMoqException._();
  static LiftRetVal<TimeOverflowMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(TimeOverflowMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 10);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class LogLevelMoqException extends MoqException {
  final String reason;
  LogLevelMoqException(String this.reason);
  LogLevelMoqException._(String this.reason);
  static LiftRetVal<LogLevelMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(LogLevelMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 11);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class TaskMoqException extends MoqException {
  final String reason;
  TaskMoqException(String this.reason);
  TaskMoqException._(String this.reason);
  static LiftRetVal<TaskMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(TaskMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 12);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class JsonMoqException extends MoqException {
  final String reason;
  JsonMoqException(String this.reason);
  JsonMoqException._(String this.reason);
  static LiftRetVal<JsonMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(JsonMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 13);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class CancelledMoqException extends MoqException {
  CancelledMoqException();
  CancelledMoqException._();
  static LiftRetVal<CancelledMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(CancelledMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 14);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class ClosedMoqException extends MoqException {
  ClosedMoqException();
  ClosedMoqException._();
  static LiftRetVal<ClosedMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(ClosedMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 15);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class BusyMoqException extends MoqException {
  BusyMoqException();
  BusyMoqException._();
  static LiftRetVal<BusyMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(BusyMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 16);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class ConnectMoqException extends MoqException {
  final String reason;
  ConnectMoqException(String this.reason);
  ConnectMoqException._(String this.reason);
  static LiftRetVal<ConnectMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(ConnectMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 17);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class BindMoqException extends MoqException {
  final String reason;
  BindMoqException(String this.reason);
  BindMoqException._(String this.reason);
  static LiftRetVal<BindMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(BindMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 18);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class RejectMoqException extends MoqException {
  final String reason;
  RejectMoqException(String this.reason);
  RejectMoqException._(String this.reason);
  static LiftRetVal<RejectMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(RejectMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 19);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class AlreadyRespondedMoqException extends MoqException {
  AlreadyRespondedMoqException();
  AlreadyRespondedMoqException._();
  static LiftRetVal<AlreadyRespondedMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(AlreadyRespondedMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 20);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class CodecMoqException extends MoqException {
  final String reason;
  CodecMoqException(String this.reason);
  CodecMoqException._(String this.reason);
  static LiftRetVal<CodecMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(CodecMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 21);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class UnauthorizedMoqException extends MoqException {
  UnauthorizedMoqException();
  UnauthorizedMoqException._();
  static LiftRetVal<UnauthorizedMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(UnauthorizedMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 22);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class ForbiddenMoqException extends MoqException {
  ForbiddenMoqException();
  ForbiddenMoqException._();
  static LiftRetVal<ForbiddenMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(ForbiddenMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 23);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class ExpiredMoqException extends MoqException {
  ExpiredMoqException();
  ExpiredMoqException._();
  static LiftRetVal<ExpiredMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(ExpiredMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 24);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class NotFoundMoqException extends MoqException {
  NotFoundMoqException();
  NotFoundMoqException._();
  static LiftRetVal<NotFoundMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(NotFoundMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 25);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class UnsupportedMoqException extends MoqException {
  UnsupportedMoqException();
  UnsupportedMoqException._();
  static LiftRetVal<UnsupportedMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(UnsupportedMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 26);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class AlreadyCommittedMoqException extends MoqException {
  AlreadyCommittedMoqException();
  AlreadyCommittedMoqException._();
  static LiftRetVal<AlreadyCommittedMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(AlreadyCommittedMoqException._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 27);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class InvalidPatternMoqException extends MoqException {
  final String reason;
  InvalidPatternMoqException(String this.reason);
  InvalidPatternMoqException._(String this.reason);
  static LiftRetVal<InvalidPatternMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(InvalidPatternMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 28);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class InvalidEpochMoqException extends MoqException {
  final String reason;
  InvalidEpochMoqException(String this.reason);
  InvalidEpochMoqException._(String this.reason);
  static LiftRetVal<InvalidEpochMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(InvalidEpochMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 29);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class UnresolvableBroadcastMoqException extends MoqException {
  final String reason;
  UnresolvableBroadcastMoqException(String this.reason);
  UnresolvableBroadcastMoqException._(String this.reason);
  static LiftRetVal<UnresolvableBroadcastMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(UnresolvableBroadcastMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 30);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class LogMoqException extends MoqException {
  final String reason;
  LogMoqException(String this.reason);
  LogMoqException._(String this.reason);
  static LiftRetVal<LogMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(LogMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 31);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class ConfigMoqException extends MoqException {
  final String reason;
  ConfigMoqException(String this.reason);
  ConfigMoqException._(String this.reason);
  static LiftRetVal<ConfigMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final reason_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final reason = reason_lifted.value;
    new_offset += reason_lifted.bytesRead;
    return LiftRetVal(ConfigMoqException._(reason), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(reason) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 32);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      reason,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class UnsupportedCatalogMoqException extends MoqException {
  final int version;
  UnsupportedCatalogMoqException(int this.version);
  UnsupportedCatalogMoqException._(int this.version);
  static LiftRetVal<UnsupportedCatalogMoqException> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final version_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final version = version_lifted.value;
    new_offset += version_lifted.bytesRead;
    return LiftRetVal(UnsupportedCatalogMoqException._(version), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(version) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 33);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      version,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
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

enum MoqErrorScope { session, stream }

class FfiConverterMoqErrorScope {
  static LiftRetVal<MoqErrorScope> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqErrorScope.session, 4);
      case 2:
        return LiftRetVal(MoqErrorScope.stream, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqErrorScope lift(RustBuffer buffer) {
    return FfiConverterMoqErrorScope.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqErrorScope input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqErrorScope _value) {
    return 4;
  }

  static int write(MoqErrorScope value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

enum MoqProtocolKind {
  cancel,
  internal,
  unauthorized,
  protocolViolation,
  keyValueFormatting,
  goawayTimeout,
  timeout,
  version,
  deliveryTimeout,
  sessionClosed,
  goingAway,
  tooFarBehind,
  malformedTrack,
  notFound,
  unroutable,
  old,
  evicted,
  wrongSize,
  frameTooLarge,
  timestampMismatch,
  app,
  unknown,
  tooManyRequests,
  expired,
  authTokenCacheOverflow,
  malformedAuthToken,
  unknownAuthTokenAlias,
  invalidRequestId,
  invalidPath,
  invalidAuthority,
  tooManyRequestUpdates,
}

class FfiConverterMoqProtocolKind {
  static LiftRetVal<MoqProtocolKind> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqProtocolKind.cancel, 4);
      case 2:
        return LiftRetVal(MoqProtocolKind.internal, 4);
      case 3:
        return LiftRetVal(MoqProtocolKind.unauthorized, 4);
      case 4:
        return LiftRetVal(MoqProtocolKind.protocolViolation, 4);
      case 5:
        return LiftRetVal(MoqProtocolKind.keyValueFormatting, 4);
      case 6:
        return LiftRetVal(MoqProtocolKind.goawayTimeout, 4);
      case 7:
        return LiftRetVal(MoqProtocolKind.timeout, 4);
      case 8:
        return LiftRetVal(MoqProtocolKind.version, 4);
      case 9:
        return LiftRetVal(MoqProtocolKind.deliveryTimeout, 4);
      case 10:
        return LiftRetVal(MoqProtocolKind.sessionClosed, 4);
      case 11:
        return LiftRetVal(MoqProtocolKind.goingAway, 4);
      case 12:
        return LiftRetVal(MoqProtocolKind.tooFarBehind, 4);
      case 13:
        return LiftRetVal(MoqProtocolKind.malformedTrack, 4);
      case 14:
        return LiftRetVal(MoqProtocolKind.notFound, 4);
      case 15:
        return LiftRetVal(MoqProtocolKind.unroutable, 4);
      case 16:
        return LiftRetVal(MoqProtocolKind.old, 4);
      case 17:
        return LiftRetVal(MoqProtocolKind.evicted, 4);
      case 18:
        return LiftRetVal(MoqProtocolKind.wrongSize, 4);
      case 19:
        return LiftRetVal(MoqProtocolKind.frameTooLarge, 4);
      case 20:
        return LiftRetVal(MoqProtocolKind.timestampMismatch, 4);
      case 21:
        return LiftRetVal(MoqProtocolKind.app, 4);
      case 22:
        return LiftRetVal(MoqProtocolKind.unknown, 4);
      case 23:
        return LiftRetVal(MoqProtocolKind.tooManyRequests, 4);
      case 24:
        return LiftRetVal(MoqProtocolKind.expired, 4);
      case 25:
        return LiftRetVal(MoqProtocolKind.authTokenCacheOverflow, 4);
      case 26:
        return LiftRetVal(MoqProtocolKind.malformedAuthToken, 4);
      case 27:
        return LiftRetVal(MoqProtocolKind.unknownAuthTokenAlias, 4);
      case 28:
        return LiftRetVal(MoqProtocolKind.invalidRequestId, 4);
      case 29:
        return LiftRetVal(MoqProtocolKind.invalidPath, 4);
      case 30:
        return LiftRetVal(MoqProtocolKind.invalidAuthority, 4);
      case 31:
        return LiftRetVal(MoqProtocolKind.tooManyRequestUpdates, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqProtocolKind lift(RustBuffer buffer) {
    return FfiConverterMoqProtocolKind.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqProtocolKind input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqProtocolKind _value) {
    return 4;
  }

  static int write(MoqProtocolKind value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

abstract class MoqJsonWindowEvent {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqJsonWindowEvent {
  static MoqJsonWindowEvent lift(RustBuffer buffer) {
    return FfiConverterMoqJsonWindowEvent.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqJsonWindowEvent> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = PushMoqJsonWindowEvent.read(subview);
        return LiftRetVal<MoqJsonWindowEvent>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = PopMoqJsonWindowEvent.read(subview);
        return LiftRetVal<MoqJsonWindowEvent>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 3:
        final lifted = SkipMoqJsonWindowEvent.read(subview);
        return LiftRetVal<MoqJsonWindowEvent>(
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

  static RustBuffer lower(MoqJsonWindowEvent value) {
    return value.lower();
  }

  static int allocationSize(MoqJsonWindowEvent value) {
    return value.allocationSize();
  }

  static int write(MoqJsonWindowEvent value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class PushMoqJsonWindowEvent extends MoqJsonWindowEvent {
  final int index;
  final String value;
  PushMoqJsonWindowEvent({required int this.index, required String this.value});
  PushMoqJsonWindowEvent._(int this.index, String this.value);
  static LiftRetVal<PushMoqJsonWindowEvent> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final index_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final index = index_lifted.value;
    new_offset += index_lifted.bytesRead;
    final value_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final value = value_lifted.value;
    new_offset += value_lifted.bytesRead;
    return LiftRetVal(PushMoqJsonWindowEvent._(index, value), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(index) +
        FfiConverterString.allocationSize(value) +
        4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      index,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterString.write(
      value,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class PopMoqJsonWindowEvent extends MoqJsonWindowEvent {
  final int start;
  final int end;
  PopMoqJsonWindowEvent({required int this.start, required int this.end});
  PopMoqJsonWindowEvent._(int this.start, int this.end);
  static LiftRetVal<PopMoqJsonWindowEvent> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final start_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final start = start_lifted.value;
    new_offset += start_lifted.bytesRead;
    final end_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final end = end_lifted.value;
    new_offset += end_lifted.bytesRead;
    return LiftRetVal(PopMoqJsonWindowEvent._(start, end), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(start) +
        FfiConverterUInt64.allocationSize(end) +
        4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      start,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      end,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class SkipMoqJsonWindowEvent extends MoqJsonWindowEvent {
  final int start;
  final int end;
  SkipMoqJsonWindowEvent({required int this.start, required int this.end});
  SkipMoqJsonWindowEvent._(int this.start, int this.end);
  static LiftRetVal<SkipMoqJsonWindowEvent> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final start_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final start = start_lifted.value;
    new_offset += start_lifted.bytesRead;
    final end_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final end = end_lifted.value;
    new_offset += end_lifted.bytesRead;
    return LiftRetVal(SkipMoqJsonWindowEvent._(start, end), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(start) +
        FfiConverterUInt64.allocationSize(end) +
        4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 3);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      start,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterUInt64.write(
      end,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

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

abstract class MoqMediaRead {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqMediaRead {
  static MoqMediaRead lift(RustBuffer buffer) {
    return FfiConverterMoqMediaRead.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqMediaRead> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = FrameMoqMediaRead.read(subview);
        return LiftRetVal<MoqMediaRead>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = GapMoqMediaRead.read(subview);
        return LiftRetVal<MoqMediaRead>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 3:
        final lifted = EndMoqMediaRead.read(subview);
        return LiftRetVal<MoqMediaRead>(
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

  static RustBuffer lower(MoqMediaRead value) {
    return value.lower();
  }

  static int allocationSize(MoqMediaRead value) {
    return value.allocationSize();
  }

  static int write(MoqMediaRead value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class FrameMoqMediaRead extends MoqMediaRead {
  final MoqMediaFrame frame;
  FrameMoqMediaRead(MoqMediaFrame this.frame);
  FrameMoqMediaRead._(MoqMediaFrame this.frame);
  static LiftRetVal<FrameMoqMediaRead> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final frame_lifted = FfiConverterMoqMediaFrame.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final frame = frame_lifted.value;
    new_offset += frame_lifted.bytesRead;
    return LiftRetVal(FrameMoqMediaRead._(frame), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterMoqMediaFrame.allocationSize(frame) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterMoqMediaFrame.write(
      frame,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class GapMoqMediaRead extends MoqMediaRead {
  final int timestampUs;
  final Duration duration;
  GapMoqMediaRead({
    required int this.timestampUs,
    required Duration this.duration,
  });
  GapMoqMediaRead._(int this.timestampUs, Duration this.duration);
  static LiftRetVal<GapMoqMediaRead> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    final duration_lifted = FfiConverterDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final duration = duration_lifted.value;
    new_offset += duration_lifted.bytesRead;
    return LiftRetVal(GapMoqMediaRead._(timestampUs, duration), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(timestampUs) +
        FfiConverterDuration.allocationSize(duration) +
        4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterDuration.write(
      duration,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class EndMoqMediaRead extends MoqMediaRead {
  final int endUs;
  EndMoqMediaRead(int this.endUs);
  EndMoqMediaRead._(int this.endUs);
  static LiftRetVal<EndMoqMediaRead> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final endUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final endUs = endUs_lifted.value;
    new_offset += endUs_lifted.bytesRead;
    return LiftRetVal(EndMoqMediaRead._(endUs), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(endUs) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 3);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      endUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

abstract class MoqMediaTarget {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqMediaTarget {
  static MoqMediaTarget lift(RustBuffer buffer) {
    return FfiConverterMoqMediaTarget.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqMediaTarget> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = NamedMoqMediaTarget.read(subview);
        return LiftRetVal<MoqMediaTarget>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = RequestedMoqMediaTarget.read(subview);
        return LiftRetVal<MoqMediaTarget>(
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

  static RustBuffer lower(MoqMediaTarget value) {
    return value.lower();
  }

  static int allocationSize(MoqMediaTarget value) {
    return value.allocationSize();
  }

  static int write(MoqMediaTarget value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class NamedMoqMediaTarget extends MoqMediaTarget {
  final String? name;
  NamedMoqMediaTarget(String? this.name);
  NamedMoqMediaTarget._(String? this.name);
  static LiftRetVal<NamedMoqMediaTarget> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final name_lifted = FfiConverterOptionalString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final name = name_lifted.value;
    new_offset += name_lifted.bytesRead;
    return LiftRetVal(NamedMoqMediaTarget._(name), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterOptionalString.allocationSize(name) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterOptionalString.write(
      name,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class RequestedMoqMediaTarget extends MoqMediaTarget {
  final MoqTrackRequest request;
  RequestedMoqMediaTarget(MoqTrackRequest this.request);
  RequestedMoqMediaTarget._(MoqTrackRequest this.request);
  static LiftRetVal<RequestedMoqMediaTarget> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final request_lifted = FfiConverterMoqTrackRequest.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final request = request_lifted.value;
    new_offset += request_lifted.bytesRead;
    return LiftRetVal(RequestedMoqMediaTarget._(request), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterMoqTrackRequest.allocationSize(request) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterMoqTrackRequest.write(
      request,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

abstract class MoqRendition {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqRendition {
  static MoqRendition lift(RustBuffer buffer) {
    return FfiConverterMoqRendition.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqRendition> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = VideoMoqRendition.read(subview);
        return LiftRetVal<MoqRendition>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = AudioMoqRendition.read(subview);
        return LiftRetVal<MoqRendition>(
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

  static RustBuffer lower(MoqRendition value) {
    return value.lower();
  }

  static int allocationSize(MoqRendition value) {
    return value.allocationSize();
  }

  static int write(MoqRendition value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class VideoMoqRendition extends MoqRendition {
  final String id;
  final MoqVideo config;
  VideoMoqRendition({required String this.id, required MoqVideo this.config});
  VideoMoqRendition._(String this.id, MoqVideo this.config);
  static LiftRetVal<VideoMoqRendition> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final id_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final id = id_lifted.value;
    new_offset += id_lifted.bytesRead;
    final config_lifted = FfiConverterMoqVideo.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final config = config_lifted.value;
    new_offset += config_lifted.bytesRead;
    return LiftRetVal(VideoMoqRendition._(id, config), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(id) +
        FfiConverterMoqVideo.allocationSize(config) +
        4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      id,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqVideo.write(
      config,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class AudioMoqRendition extends MoqRendition {
  final String id;
  final MoqAudio config;
  AudioMoqRendition({required String this.id, required MoqAudio this.config});
  AudioMoqRendition._(String this.id, MoqAudio this.config);
  static LiftRetVal<AudioMoqRendition> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final id_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final id = id_lifted.value;
    new_offset += id_lifted.bytesRead;
    final config_lifted = FfiConverterMoqAudio.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final config = config_lifted.value;
    new_offset += config_lifted.bytesRead;
    return LiftRetVal(AudioMoqRendition._(id, config), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(id) +
        FfiConverterMoqAudio.allocationSize(config) +
        4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      id,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterMoqAudio.write(
      config,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
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

abstract class MoqAnnounceEvent {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqAnnounceEvent {
  static MoqAnnounceEvent lift(RustBuffer buffer) {
    return FfiConverterMoqAnnounceEvent.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqAnnounceEvent> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = StartMoqAnnounceEvent.read(subview);
        return LiftRetVal<MoqAnnounceEvent>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = UpdateMoqAnnounceEvent.read(subview);
        return LiftRetVal<MoqAnnounceEvent>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 3:
        final lifted = EndMoqAnnounceEvent.read(subview);
        return LiftRetVal<MoqAnnounceEvent>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 4:
        final lifted = RestartMoqAnnounceEvent.read(subview);
        return LiftRetVal<MoqAnnounceEvent>(
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

  static RustBuffer lower(MoqAnnounceEvent value) {
    return value.lower();
  }

  static int allocationSize(MoqAnnounceEvent value) {
    return value.allocationSize();
  }

  static int write(MoqAnnounceEvent value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class StartMoqAnnounceEvent extends MoqAnnounceEvent {
  final MoqAnnounce announce;
  StartMoqAnnounceEvent(MoqAnnounce this.announce);
  StartMoqAnnounceEvent._(MoqAnnounce this.announce);
  static LiftRetVal<StartMoqAnnounceEvent> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final announce_lifted = FfiConverterMoqAnnounce.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final announce = announce_lifted.value;
    new_offset += announce_lifted.bytesRead;
    return LiftRetVal(StartMoqAnnounceEvent._(announce), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterMoqAnnounce.allocationSize(announce) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterMoqAnnounce.write(
      announce,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class UpdateMoqAnnounceEvent extends MoqAnnounceEvent {
  final MoqAnnounce announce;
  UpdateMoqAnnounceEvent(MoqAnnounce this.announce);
  UpdateMoqAnnounceEvent._(MoqAnnounce this.announce);
  static LiftRetVal<UpdateMoqAnnounceEvent> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final announce_lifted = FfiConverterMoqAnnounce.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final announce = announce_lifted.value;
    new_offset += announce_lifted.bytesRead;
    return LiftRetVal(UpdateMoqAnnounceEvent._(announce), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterMoqAnnounce.allocationSize(announce) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterMoqAnnounce.write(
      announce,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class EndMoqAnnounceEvent extends MoqAnnounceEvent {
  final MoqAnnounce announce;
  EndMoqAnnounceEvent(MoqAnnounce this.announce);
  EndMoqAnnounceEvent._(MoqAnnounce this.announce);
  static LiftRetVal<EndMoqAnnounceEvent> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final announce_lifted = FfiConverterMoqAnnounce.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final announce = announce_lifted.value;
    new_offset += announce_lifted.bytesRead;
    return LiftRetVal(EndMoqAnnounceEvent._(announce), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterMoqAnnounce.allocationSize(announce) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 3);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterMoqAnnounce.write(
      announce,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class RestartMoqAnnounceEvent extends MoqAnnounceEvent {
  final MoqAnnounce announce;
  RestartMoqAnnounceEvent(MoqAnnounce this.announce);
  RestartMoqAnnounceEvent._(MoqAnnounce this.announce);
  static LiftRetVal<RestartMoqAnnounceEvent> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final announce_lifted = FfiConverterMoqAnnounce.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final announce = announce_lifted.value;
    new_offset += announce_lifted.bytesRead;
    return LiftRetVal(RestartMoqAnnounceEvent._(announce), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterMoqAnnounce.allocationSize(announce) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 4);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterMoqAnnounce.write(
      announce,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

enum MoqTransport { quic, iroh, webSocket, tcp, unix, webTransport }

class FfiConverterMoqTransport {
  static LiftRetVal<MoqTransport> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqTransport.quic, 4);
      case 2:
        return LiftRetVal(MoqTransport.iroh, 4);
      case 3:
        return LiftRetVal(MoqTransport.webSocket, 4);
      case 4:
        return LiftRetVal(MoqTransport.tcp, 4);
      case 5:
        return LiftRetVal(MoqTransport.unix, 4);
      case 6:
        return LiftRetVal(MoqTransport.webTransport, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqTransport lift(RustBuffer buffer) {
    return FfiConverterMoqTransport.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqTransport input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqTransport _value) {
    return 4;
  }

  static int write(MoqTransport value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

enum MoqCongestionControl { loss, delay, none }

class FfiConverterMoqCongestionControl {
  static LiftRetVal<MoqCongestionControl> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqCongestionControl.loss, 4);
      case 2:
        return LiftRetVal(MoqCongestionControl.delay, 4);
      case 3:
        return LiftRetVal(MoqCongestionControl.none, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqCongestionControl lift(RustBuffer buffer) {
    return FfiConverterMoqCongestionControl.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqCongestionControl input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqCongestionControl _value) {
    return 4;
  }

  static int write(MoqCongestionControl value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

enum MoqConnectionPhase {
  connecting,
  connected,
  disconnected,
  migrating,
  closed,
}

class FfiConverterMoqConnectionPhase {
  static LiftRetVal<MoqConnectionPhase> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqConnectionPhase.connecting, 4);
      case 2:
        return LiftRetVal(MoqConnectionPhase.connected, 4);
      case 3:
        return LiftRetVal(MoqConnectionPhase.disconnected, 4);
      case 4:
        return LiftRetVal(MoqConnectionPhase.migrating, 4);
      case 5:
        return LiftRetVal(MoqConnectionPhase.closed, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqConnectionPhase lift(RustBuffer buffer) {
    return FfiConverterMoqConnectionPhase.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqConnectionPhase input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqConnectionPhase _value) {
    return 4;
  }

  static int write(MoqConnectionPhase value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

enum MoqVideoCodec { h264, h265 }

class FfiConverterMoqVideoCodec {
  static LiftRetVal<MoqVideoCodec> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqVideoCodec.h264, 4);
      case 2:
        return LiftRetVal(MoqVideoCodec.h265, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqVideoCodec lift(RustBuffer buffer) {
    return FfiConverterMoqVideoCodec.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqVideoCodec input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqVideoCodec _value) {
    return 4;
  }

  static int write(MoqVideoCodec value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

abstract class MoqVideoEncoderKind {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqVideoEncoderKind {
  static MoqVideoEncoderKind lift(RustBuffer buffer) {
    return FfiConverterMoqVideoEncoderKind.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoEncoderKind> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = AutoMoqVideoEncoderKind.read(subview);
        return LiftRetVal<MoqVideoEncoderKind>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = HardwareMoqVideoEncoderKind.read(subview);
        return LiftRetVal<MoqVideoEncoderKind>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 3:
        final lifted = SoftwareMoqVideoEncoderKind.read(subview);
        return LiftRetVal<MoqVideoEncoderKind>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 4:
        final lifted = NamedMoqVideoEncoderKind.read(subview);
        return LiftRetVal<MoqVideoEncoderKind>(
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

  static RustBuffer lower(MoqVideoEncoderKind value) {
    return value.lower();
  }

  static int allocationSize(MoqVideoEncoderKind value) {
    return value.allocationSize();
  }

  static int write(MoqVideoEncoderKind value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class AutoMoqVideoEncoderKind extends MoqVideoEncoderKind {
  AutoMoqVideoEncoderKind();
  AutoMoqVideoEncoderKind._();
  static LiftRetVal<AutoMoqVideoEncoderKind> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(AutoMoqVideoEncoderKind._(), new_offset);
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

class HardwareMoqVideoEncoderKind extends MoqVideoEncoderKind {
  HardwareMoqVideoEncoderKind();
  HardwareMoqVideoEncoderKind._();
  static LiftRetVal<HardwareMoqVideoEncoderKind> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(HardwareMoqVideoEncoderKind._(), new_offset);
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
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    return new_offset;
  }
}

class SoftwareMoqVideoEncoderKind extends MoqVideoEncoderKind {
  SoftwareMoqVideoEncoderKind();
  SoftwareMoqVideoEncoderKind._();
  static LiftRetVal<SoftwareMoqVideoEncoderKind> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    return LiftRetVal(SoftwareMoqVideoEncoderKind._(), new_offset);
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

class NamedMoqVideoEncoderKind extends MoqVideoEncoderKind {
  final String name;
  NamedMoqVideoEncoderKind(String this.name);
  NamedMoqVideoEncoderKind._(String this.name);
  static LiftRetVal<NamedMoqVideoEncoderKind> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final name_lifted = FfiConverterString.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final name = name_lifted.value;
    new_offset += name_lifted.bytesRead;
    return LiftRetVal(NamedMoqVideoEncoderKind._(name), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterString.allocationSize(name) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 4);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterString.write(
      name,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

abstract class MoqVideoGop {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqVideoGop {
  static MoqVideoGop lift(RustBuffer buffer) {
    return FfiConverterMoqVideoGop.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoGop> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = KeyframeMoqVideoGop.read(subview);
        return LiftRetVal<MoqVideoGop>(
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

  static RustBuffer lower(MoqVideoGop value) {
    return value.lower();
  }

  static int allocationSize(MoqVideoGop value) {
    return value.allocationSize();
  }

  static int write(MoqVideoGop value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class KeyframeMoqVideoGop extends MoqVideoGop {
  final int interval;
  KeyframeMoqVideoGop(int this.interval);
  KeyframeMoqVideoGop._(int this.interval);
  static LiftRetVal<KeyframeMoqVideoGop> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final interval_lifted = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final interval = interval_lifted.value;
    new_offset += interval_lifted.bytesRead;
    return LiftRetVal(KeyframeMoqVideoGop._(interval), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt32.allocationSize(interval) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt32.write(
      interval,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

enum MoqVideoPixelFormat { i420, rgba }

class FfiConverterMoqVideoPixelFormat {
  static LiftRetVal<MoqVideoPixelFormat> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    switch (index) {
      case 1:
        return LiftRetVal(MoqVideoPixelFormat.i420, 4);
      case 2:
        return LiftRetVal(MoqVideoPixelFormat.rgba, 4);
      default:
        throw UniffiInternalError(
          UniffiInternalError.unexpectedEnumCase,
          "Unable to determine enum variant",
        );
    }
  }

  static MoqVideoPixelFormat lift(RustBuffer buffer) {
    return FfiConverterMoqVideoPixelFormat.read(buffer.asUint8List()).value;
  }

  static RustBuffer lower(MoqVideoPixelFormat input) {
    return toRustBuffer(createUint8ListFromInt(input.index + 1));
  }

  static int allocationSize(MoqVideoPixelFormat _value) {
    return 4;
  }

  static int write(MoqVideoPixelFormat value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.index + 1);
    return 4;
  }
}

abstract class MoqVideoRead {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqVideoRead {
  static MoqVideoRead lift(RustBuffer buffer) {
    return FfiConverterMoqVideoRead.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoRead> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = FrameMoqVideoRead.read(subview);
        return LiftRetVal<MoqVideoRead>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 2:
        final lifted = GapMoqVideoRead.read(subview);
        return LiftRetVal<MoqVideoRead>(
          lifted.value,
          lifted.bytesRead - subview.offsetInBytes + 4,
        );
      case 3:
        final lifted = EndMoqVideoRead.read(subview);
        return LiftRetVal<MoqVideoRead>(
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

  static RustBuffer lower(MoqVideoRead value) {
    return value.lower();
  }

  static int allocationSize(MoqVideoRead value) {
    return value.allocationSize();
  }

  static int write(MoqVideoRead value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class FrameMoqVideoRead extends MoqVideoRead {
  final MoqVideoDecodedFrame frame;
  FrameMoqVideoRead(MoqVideoDecodedFrame this.frame);
  FrameMoqVideoRead._(MoqVideoDecodedFrame this.frame);
  static LiftRetVal<FrameMoqVideoRead> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final frame_lifted = FfiConverterMoqVideoDecodedFrame.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final frame = frame_lifted.value;
    new_offset += frame_lifted.bytesRead;
    return LiftRetVal(FrameMoqVideoRead._(frame), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterMoqVideoDecodedFrame.allocationSize(frame) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterMoqVideoDecodedFrame.write(
      frame,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class GapMoqVideoRead extends MoqVideoRead {
  final int timestampUs;
  final Duration duration;
  GapMoqVideoRead({
    required int this.timestampUs,
    required Duration this.duration,
  });
  GapMoqVideoRead._(int this.timestampUs, Duration this.duration);
  static LiftRetVal<GapMoqVideoRead> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final timestampUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final timestampUs = timestampUs_lifted.value;
    new_offset += timestampUs_lifted.bytesRead;
    final duration_lifted = FfiConverterDuration.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final duration = duration_lifted.value;
    new_offset += duration_lifted.bytesRead;
    return LiftRetVal(GapMoqVideoRead._(timestampUs, duration), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(timestampUs) +
        FfiConverterDuration.allocationSize(duration) +
        4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 2);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      timestampUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    new_offset += FfiConverterDuration.write(
      duration,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

class EndMoqVideoRead extends MoqVideoRead {
  final int endUs;
  EndMoqVideoRead(int this.endUs);
  EndMoqVideoRead._(int this.endUs);
  static LiftRetVal<EndMoqVideoRead> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final endUs_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final endUs = endUs_lifted.value;
    new_offset += endUs_lifted.bytesRead;
    return LiftRetVal(EndMoqVideoRead._(endUs), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(endUs) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 3);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      endUs,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

abstract class MoqVideoSurface {
  RustBuffer lower();
  int allocationSize();
  int write(Uint8List buf);
}

class FfiConverterMoqVideoSurface {
  static MoqVideoSurface lift(RustBuffer buffer) {
    return FfiConverterMoqVideoSurface.read(buffer.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoSurface> read(Uint8List buf) {
    final index = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final subview = Uint8List.view(buf.buffer, buf.offsetInBytes + 4);
    switch (index) {
      case 1:
        final lifted = PixelBufferMoqVideoSurface.read(subview);
        return LiftRetVal<MoqVideoSurface>(
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

  static RustBuffer lower(MoqVideoSurface value) {
    return value.lower();
  }

  static int allocationSize(MoqVideoSurface value) {
    return value.allocationSize();
  }

  static int write(MoqVideoSurface value, Uint8List buf) {
    return value.write(buf) - buf.offsetInBytes;
  }
}

class PixelBufferMoqVideoSurface extends MoqVideoSurface {
  final int pointer;
  PixelBufferMoqVideoSurface(int this.pointer);
  PixelBufferMoqVideoSurface._(int this.pointer);
  static LiftRetVal<PixelBufferMoqVideoSurface> read(Uint8List buf) {
    int new_offset = buf.offsetInBytes;
    final pointer_lifted = FfiConverterUInt64.read(
      Uint8List.view(buf.buffer, new_offset),
    );
    final pointer = pointer_lifted.value;
    new_offset += pointer_lifted.bytesRead;
    return LiftRetVal(PixelBufferMoqVideoSurface._(pointer), new_offset);
  }

  @override
  RustBuffer lower() {
    final buf = Uint8List(allocationSize());
    write(buf);
    return toRustBuffer(buf);
  }

  @override
  int allocationSize() {
    return FfiConverterUInt64.allocationSize(pointer) + 4;
  }

  @override
  int write(Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, 1);
    int new_offset = buf.offsetInBytes + 4;
    new_offset += FfiConverterUInt64.write(
      pointer,
      Uint8List.view(buf.buffer, new_offset),
    );
    return new_offset;
  }
}

abstract class MoqAudioCodecInterface {}

final _MoqAudioCodecFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqaudiocodec(ptr, status));
});

class MoqAudioCodec implements MoqAudioCodecInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqAudioCodec._(this._ptr) {
    _MoqAudioCodecFinalizer.attach(this, _ptr, detach: this);
  }
  MoqAudioCodec.aac()
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqaudiocodec_aac(status),
        null,
      ) {
    _MoqAudioCodecFinalizer.attach(this, _ptr, detach: this);
  }
  MoqAudioCodec.opus()
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqaudiocodec_opus(status),
        null,
      ) {
    _MoqAudioCodecFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAudioCodec.lift(Pointer<Void> ptr) {
    return MoqAudioCodec._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqaudiocodec(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqAudioCodecFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqaudiocodec(_ptr, status));
  }
}

class FfiConverterMoqAudioCodec {
  static MoqAudioCodec lift(Pointer<Void> ptr) {
    return MoqAudioCodec.lift(ptr);
  }

  static Pointer<Void> lower(MoqAudioCodec value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAudioCodec value) {
    return 8;
  }

  static LiftRetVal<MoqAudioCodec> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAudioCodec.lift(pointer), 8);
  }

  static int write(MoqAudioCodec value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAudioConsumerInterface {
  void cancel();
  Duration delay();
  UniffiRustFuture<MoqAudioRead?> next();
}

final _MoqAudioConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqaudioconsumer(ptr, status));
});

class MoqAudioConsumer implements MoqAudioConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqAudioConsumer._(this._ptr) {
    _MoqAudioConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  static UniffiRustFuture<MoqAudioConsumer> subscribe({
    required MoqBroadcastConsumer broadcast,
    required String id,
    required MoqAudio catalogAudio,
    required MoqAudioDecoderOutput output,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_constructor_moqaudioconsumer_subscribe(
        FfiConverterMoqBroadcastConsumer.lower(broadcast),
        FfiConverterString.lower(id),
        FfiConverterMoqAudio.lower(catalogAudio),
        FfiConverterMoqAudioDecoderOutput.lower(output),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (int handle) => MoqAudioConsumer._(Pointer<Void>.fromAddress(handle)),
      moqExceptionErrorHandler,
    );
  }

  factory MoqAudioConsumer.lift(Pointer<Void> ptr) {
    return MoqAudioConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqaudioconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqAudioConsumerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqaudioconsumer(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqaudioconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  Duration delay() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqaudioconsumer_delay(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterDuration.lift,
      null,
    );
  }

  UniffiRustFuture<MoqAudioRead?> next() {
    return uniffiRustCallAsync(
      () =>
          uniffi_moq_ffi_fn_method_moqaudioconsumer_next(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqAudioRead.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqAudioConsumer {
  static MoqAudioConsumer lift(Pointer<Void> ptr) {
    return MoqAudioConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqAudioConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAudioConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqAudioConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAudioConsumer.lift(pointer), 8);
  }

  static int write(MoqAudioConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAudioEncoderInterface {
  MoqAudioPacket encode({required List<double> pcm});
  MoqAudioEncoderFinished finish({required List<double> pcm});
  int frameSize();
  void setBitrate({required int bitrate});
  MoqAudioInit trackInit();
}

final _MoqAudioEncoderFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqaudioencoder(ptr, status));
});

class MoqAudioEncoder implements MoqAudioEncoderInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqAudioEncoder._(this._ptr) {
    _MoqAudioEncoderFinalizer.attach(this, _ptr, detach: this);
  }
  MoqAudioEncoder({required MoqAudioEncoderOutput output})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqaudioencoder_new(
          FfiConverterMoqAudioEncoderOutput.lower(output),
          status,
        ),
        moqExceptionErrorHandler,
      ) {
    _MoqAudioEncoderFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAudioEncoder.lift(Pointer<Void> ptr) {
    return MoqAudioEncoder._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqaudioencoder(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqAudioEncoderFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqaudioencoder(_ptr, status));
  }

  MoqAudioPacket encode({required List<double> pcm}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqaudioencoder_encode(
        uniffiClonePointer(),
        FfiConverterSequenceDouble32.lower(pcm),
        status,
      ),
      FfiConverterMoqAudioPacket.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqAudioEncoderFinished finish({required List<double> pcm}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqaudioencoder_finish(
        uniffiClonePointer(),
        FfiConverterSequenceDouble32.lower(pcm),
        status,
      ),
      FfiConverterMoqAudioEncoderFinished.lift,
      moqExceptionErrorHandler,
    );
  }

  int frameSize() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqaudioencoder_frame_size(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      moqExceptionErrorHandler,
    );
  }

  void setBitrate({required int bitrate}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqaudioencoder_set_bitrate(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(bitrate),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqAudioInit trackInit() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqaudioencoder_track_init(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqAudioInit.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqAudioEncoder {
  static MoqAudioEncoder lift(Pointer<Void> ptr) {
    return MoqAudioEncoder.lift(ptr);
  }

  static Pointer<Void> lower(MoqAudioEncoder value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAudioEncoder value) {
    return 8;
  }

  static LiftRetVal<MoqAudioEncoder> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAudioEncoder.lift(pointer), 8);
  }

  static int write(MoqAudioEncoder value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAudioProducerInterface {
  MoqTrackDemand demand();
  void finish();
  void reanchor();
  MoqReservation? reservation();
  void write({required MoqAudioFrame frame});
}

final _MoqAudioProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqaudioproducer(ptr, status));
});

class MoqAudioProducer implements MoqAudioProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqAudioProducer._(this._ptr) {
    _MoqAudioProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqAudioProducer({
    required MoqMediaCatalogProducer catalog,
    required MoqAudioProducerOptions options,
  }) : _ptr = rustCall(
         (status) => uniffi_moq_ffi_fn_constructor_moqaudioproducer_new(
           FfiConverterMoqMediaCatalogProducer.lower(catalog),
           FfiConverterMoqAudioProducerOptions.lower(options),
           status,
         ),
         moqExceptionErrorHandler,
       ) {
    _MoqAudioProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAudioProducer.lift(Pointer<Void> ptr) {
    return MoqAudioProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqaudioproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqAudioProducerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqaudioproducer(_ptr, status));
  }

  MoqTrackDemand demand() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqaudioproducer_demand(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDemand.lift,
      moqExceptionErrorHandler,
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqaudioproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void reanchor() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqaudioproducer_reanchor(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqReservation? reservation() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqaudioproducer_reservation(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalMoqReservation.lift,
      null,
    );
  }

  void write({required MoqAudioFrame frame}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqaudioproducer_write(
        uniffiClonePointer(),
        FfiConverterMoqAudioFrame.lower(frame),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqAudioProducer {
  static MoqAudioProducer lift(Pointer<Void> ptr) {
    return MoqAudioProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqAudioProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAudioProducer value) {
    return 8;
  }

  static LiftRetVal<MoqAudioProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAudioProducer.lift(pointer), 8);
  }

  static int write(MoqAudioProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqGrantConsumerInterface {
  void cancel();
  UniffiRustFuture<MoqGrant?> changed();
  MoqGrant? grant();
}

final _MoqGrantConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqgrantconsumer(ptr, status));
});

class MoqGrantConsumer implements MoqGrantConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqGrantConsumer._(this._ptr) {
    _MoqGrantConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqGrantConsumer.lift(Pointer<Void> ptr) {
    return MoqGrantConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqgrantconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqGrantConsumerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqgrantconsumer(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqgrantconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  UniffiRustFuture<MoqGrant?> changed() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqgrantconsumer_changed(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqGrant.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqGrant? grant() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqgrantconsumer_grant(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalMoqGrant.lift,
      null,
    );
  }
}

class FfiConverterMoqGrantConsumer {
  static MoqGrantConsumer lift(Pointer<Void> ptr) {
    return MoqGrantConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqGrantConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqGrantConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqGrantConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqGrantConsumer.lift(pointer), 8);
  }

  static int write(MoqGrantConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAllocatorInterface {
  MoqReservation reserve({required MoqTrackDemand demand, required int maxBps});
}

final _MoqAllocatorFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqallocator(ptr, status));
});

class MoqAllocator implements MoqAllocatorInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqAllocator._(this._ptr) {
    _MoqAllocatorFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAllocator.lift(Pointer<Void> ptr) {
    return MoqAllocator._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqallocator(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqAllocatorFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqallocator(_ptr, status));
  }

  MoqReservation reserve({
    required MoqTrackDemand demand,
    required int maxBps,
  }) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqallocator_reserve(
        uniffiClonePointer(),
        FfiConverterMoqTrackDemand.lower(demand),
        FfiConverterUInt64.lower(maxBps),
        status,
      ),
      FfiConverterMoqReservation.lift,
      null,
    );
  }
}

class FfiConverterMoqAllocator {
  static MoqAllocator lift(Pointer<Void> ptr) {
    return MoqAllocator.lift(ptr);
  }

  static Pointer<Void> lower(MoqAllocator value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAllocator value) {
    return 8;
  }

  static LiftRetVal<MoqAllocator> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAllocator.lift(pointer), 8);
  }

  static int write(MoqAllocator value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqBandwidthConsumerInterface {
  void cancel();
  int? rate();
  UniffiRustFuture<int?> rateChanged();
}

final _MoqBandwidthConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqbandwidthconsumer(ptr, status),
  );
});

class MoqBandwidthConsumer implements MoqBandwidthConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqBandwidthConsumer._(this._ptr) {
    _MoqBandwidthConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqBandwidthConsumer.lift(Pointer<Void> ptr) {
    return MoqBandwidthConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbandwidthconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqBandwidthConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbandwidthconsumer(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbandwidthconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  int? rate() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbandwidthconsumer_rate(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalUInt64.lift,
      null,
    );
  }

  UniffiRustFuture<int?> rateChanged() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbandwidthconsumer_rate_changed(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalUInt64.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqBandwidthConsumer {
  static MoqBandwidthConsumer lift(Pointer<Void> ptr) {
    return MoqBandwidthConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqBandwidthConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqBandwidthConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqBandwidthConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqBandwidthConsumer.lift(pointer), 8);
  }

  static int write(MoqBandwidthConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqReservationInterface {
  int? rate();
  void update({required int maxBps});
}

final _MoqReservationFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqreservation(ptr, status));
});

class MoqReservation implements MoqReservationInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqReservation._(this._ptr) {
    _MoqReservationFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqReservation.lift(Pointer<Void> ptr) {
    return MoqReservation._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqreservation(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqReservationFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqreservation(_ptr, status));
  }

  int? rate() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqreservation_rate(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalUInt64.lift,
      null,
    );
  }

  void update({required int maxBps}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqreservation_update(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(maxBps),
        status,
      );
    }, null);
  }
}

class FfiConverterMoqReservation {
  static MoqReservation lift(Pointer<Void> ptr) {
    return MoqReservation.lift(ptr);
  }

  static Pointer<Void> lower(MoqReservation value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqReservation value) {
    return 8;
  }

  static LiftRetVal<MoqReservation> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqReservation.lift(pointer), 8);
  }

  static int write(MoqReservation value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqBroadcastConsumerInterface {
  UniffiRustFuture<MoqGroupConsumer> fetchGroup({
    required String name,
    required int sequence,
    required MoqFetchGroupOptions? options,
  });
  UniffiRustFuture<MoqBroadcastConsumer> resolve({
    required MoqRendition rendition,
  });
  UniffiRustFuture<MoqTrackConsumer> subscribeTrack({
    required String name,
    required MoqSubscription? subscription,
  });
}

final _MoqBroadcastConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqbroadcastconsumer(ptr, status),
  );
});

class MoqBroadcastConsumer implements MoqBroadcastConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqBroadcastConsumer._(this._ptr) {
    _MoqBroadcastConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqBroadcastConsumer.lift(Pointer<Void> ptr) {
    return MoqBroadcastConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbroadcastconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqBroadcastConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbroadcastconsumer(_ptr, status),
    );
  }

  UniffiRustFuture<MoqGroupConsumer> fetchGroup({
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
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqGroupConsumer.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  UniffiRustFuture<MoqBroadcastConsumer> resolve({
    required MoqRendition rendition,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastconsumer_resolve(
        uniffiClonePointer(),
        FfiConverterMoqRendition.lower(rendition),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqBroadcastConsumer.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }

  UniffiRustFuture<MoqTrackConsumer> subscribeTrack({
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
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqTrackConsumer.lift(Pointer<Void>.fromAddress(ptr)),
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

abstract class MoqGroupConsumerInterface {
  void cancel();
  UniffiRustFuture<MoqFrame?> readFrame();
  int sequence();
}

final _MoqGroupConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqgroupconsumer(ptr, status));
});

class MoqGroupConsumer implements MoqGroupConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqGroupConsumer._(this._ptr) {
    _MoqGroupConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqGroupConsumer.lift(Pointer<Void> ptr) {
    return MoqGroupConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqgroupconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
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

  UniffiRustFuture<MoqFrame?> readFrame() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqgroupconsumer_read_frame(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
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

abstract class MoqTrackConsumerInterface {
  void cancel();
  MoqTrackInfo info();
  UniffiRustFuture<MoqGroupConsumer?> nextGroup();
  UniffiRustFuture<MoqFrame?> readFrame();
  UniffiRustFuture<MoqDatagram?> recvDatagram();
  UniffiRustFuture<MoqGroupConsumer?> recvGroup();
  void update({required MoqSubscription subscription});
}

final _MoqTrackConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackconsumer(ptr, status));
});

class MoqTrackConsumer implements MoqTrackConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqTrackConsumer._(this._ptr) {
    _MoqTrackConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTrackConsumer.lift(Pointer<Void> ptr) {
    return MoqTrackConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtrackconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
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
      null,
    );
  }

  UniffiRustFuture<MoqGroupConsumer?> nextGroup() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackconsumer_next_group(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqGroupConsumer.lift,
      moqExceptionErrorHandler,
    );
  }

  UniffiRustFuture<MoqFrame?> readFrame() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackconsumer_read_frame(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqFrame.lift,
      moqExceptionErrorHandler,
    );
  }

  UniffiRustFuture<MoqDatagram?> recvDatagram() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackconsumer_recv_datagram(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqDatagram.lift,
      moqExceptionErrorHandler,
    );
  }

  UniffiRustFuture<MoqGroupConsumer?> recvGroup() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackconsumer_recv_group(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
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
    }, moqExceptionErrorHandler);
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

abstract class MoqTrackDemandInterface {
  String name();
  int? wanted();
  UniffiRustFuture<int?> wantedChanged();
}

final _MoqTrackDemandFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackdemand(ptr, status));
});

class MoqTrackDemand implements MoqTrackDemandInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqTrackDemand._(this._ptr) {
    _MoqTrackDemandFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTrackDemand.lift(Pointer<Void> ptr) {
    return MoqTrackDemand._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtrackdemand(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqTrackDemandFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackdemand(_ptr, status));
  }

  String name() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackdemand_name(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterString.lift,
      null,
    );
  }

  int? wanted() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackdemand_wanted(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalUInt64.lift,
      null,
    );
  }

  UniffiRustFuture<int?> wantedChanged() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackdemand_wanted_changed(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalUInt64.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqTrackDemand {
  static MoqTrackDemand lift(Pointer<Void> ptr) {
    return MoqTrackDemand.lift(ptr);
  }

  static Pointer<Void> lower(MoqTrackDemand value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqTrackDemand value) {
    return 8;
  }

  static LiftRetVal<MoqTrackDemand> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqTrackDemand.lift(pointer), 8);
  }

  static int write(MoqTrackDemand value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqJsonSnapshotConsumerInterface {
  void cancel();
  UniffiRustFuture<MoqJsonValue?> latest();
  UniffiRustFuture<MoqJsonValue?> next();
}

final _MoqJsonSnapshotConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonsnapshotconsumer(ptr, status),
  );
});

class MoqJsonSnapshotConsumer implements MoqJsonSnapshotConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqJsonSnapshotConsumer._(this._ptr) {
    _MoqJsonSnapshotConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqJsonSnapshotConsumer({required MoqTrackConsumer track})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqjsonsnapshotconsumer_new(
          FfiConverterMoqTrackConsumer.lower(track),
          status,
        ),
        moqExceptionErrorHandler,
      ) {
    _MoqJsonSnapshotConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonSnapshotConsumer.lift(Pointer<Void> ptr) {
    return MoqJsonSnapshotConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonsnapshotconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
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

  UniffiRustFuture<MoqJsonValue?> latest() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqjsonsnapshotconsumer_latest(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqJsonValue.lift,
      moqExceptionErrorHandler,
    );
  }

  UniffiRustFuture<MoqJsonValue?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqjsonsnapshotconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqJsonValue.lift,
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
  MoqTrackDemand demand();
  void finish();
  void update({required String value, int? timestampUs = null});
}

final _MoqJsonSnapshotProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonsnapshotproducer(ptr, status),
  );
});

class MoqJsonSnapshotProducer implements MoqJsonSnapshotProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqJsonSnapshotProducer._(this._ptr) {
    _MoqJsonSnapshotProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqJsonSnapshotProducer({
    required MoqBroadcastProducer broadcast,
    required MoqTrackProducer track,
    required MoqJsonSnapshotConfig config,
  }) : _ptr = rustCall(
         (status) => uniffi_moq_ffi_fn_constructor_moqjsonsnapshotproducer_new(
           FfiConverterMoqBroadcastProducer.lower(broadcast),
           FfiConverterMoqTrackProducer.lower(track),
           FfiConverterMoqJsonSnapshotConfig.lower(config),
           status,
         ),
         moqExceptionErrorHandler,
       ) {
    _MoqJsonSnapshotProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonSnapshotProducer.lift(Pointer<Void> ptr) {
    return MoqJsonSnapshotProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonsnapshotproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqJsonSnapshotProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqjsonsnapshotproducer(_ptr, status),
    );
  }

  MoqTrackDemand demand() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqjsonsnapshotproducer_demand(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDemand.lift,
      moqExceptionErrorHandler,
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

  void update({required String value, int? timestampUs = null}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonsnapshotproducer_update(
        uniffiClonePointer(),
        FfiConverterString.lower(value),
        FfiConverterOptionalUInt64.lower(timestampUs),
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
  UniffiRustFuture<MoqJsonValue?> next();
}

final _MoqJsonStreamConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonstreamconsumer(ptr, status),
  );
});

class MoqJsonStreamConsumer implements MoqJsonStreamConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqJsonStreamConsumer._(this._ptr) {
    _MoqJsonStreamConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqJsonStreamConsumer({required MoqTrackConsumer track})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqjsonstreamconsumer_new(
          FfiConverterMoqTrackConsumer.lower(track),
          status,
        ),
        moqExceptionErrorHandler,
      ) {
    _MoqJsonStreamConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonStreamConsumer.lift(Pointer<Void> ptr) {
    return MoqJsonStreamConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonstreamconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
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

  UniffiRustFuture<MoqJsonValue?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqjsonstreamconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqJsonValue.lift,
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
  void append({required String value, int? timestampUs = null});
  MoqTrackDemand demand();
  void finish();
}

final _MoqJsonStreamProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonstreamproducer(ptr, status),
  );
});

class MoqJsonStreamProducer implements MoqJsonStreamProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqJsonStreamProducer._(this._ptr) {
    _MoqJsonStreamProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqJsonStreamProducer({
    required MoqBroadcastProducer broadcast,
    required MoqTrackProducer track,
  }) : _ptr = rustCall(
         (status) => uniffi_moq_ffi_fn_constructor_moqjsonstreamproducer_new(
           FfiConverterMoqBroadcastProducer.lower(broadcast),
           FfiConverterMoqTrackProducer.lower(track),
           status,
         ),
         moqExceptionErrorHandler,
       ) {
    _MoqJsonStreamProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonStreamProducer.lift(Pointer<Void> ptr) {
    return MoqJsonStreamProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonstreamproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqJsonStreamProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqjsonstreamproducer(_ptr, status),
    );
  }

  void append({required String value, int? timestampUs = null}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonstreamproducer_append(
        uniffiClonePointer(),
        FfiConverterString.lower(value),
        FfiConverterOptionalUInt64.lower(timestampUs),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqTrackDemand demand() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqjsonstreamproducer_demand(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDemand.lift,
      moqExceptionErrorHandler,
    );
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

abstract class MoqJsonWindowConsumerInterface {
  void cancel();
  UniffiRustFuture<MoqJsonWindowTimed?> next();
}

final _MoqJsonWindowConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonwindowconsumer(ptr, status),
  );
});

class MoqJsonWindowConsumer implements MoqJsonWindowConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqJsonWindowConsumer._(this._ptr) {
    _MoqJsonWindowConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqJsonWindowConsumer({required MoqTrackConsumer track})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqjsonwindowconsumer_new(
          FfiConverterMoqTrackConsumer.lower(track),
          status,
        ),
        moqExceptionErrorHandler,
      ) {
    _MoqJsonWindowConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonWindowConsumer.lift(Pointer<Void> ptr) {
    return MoqJsonWindowConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonwindowconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqJsonWindowConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqjsonwindowconsumer(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonwindowconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  UniffiRustFuture<MoqJsonWindowTimed?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqjsonwindowconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqJsonWindowTimed.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqJsonWindowConsumer {
  static MoqJsonWindowConsumer lift(Pointer<Void> ptr) {
    return MoqJsonWindowConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqJsonWindowConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqJsonWindowConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqJsonWindowConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqJsonWindowConsumer.lift(pointer), 8);
  }

  static int write(MoqJsonWindowConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqJsonWindowProducerInterface {
  void cut();
  MoqTrackDemand demand();
  void finish();
  void pop({required int count, int? timestampUs = null});
  void push({required String value, int? timestampUs = null});
}

final _MoqJsonWindowProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqjsonwindowproducer(ptr, status),
  );
});

class MoqJsonWindowProducer implements MoqJsonWindowProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqJsonWindowProducer._(this._ptr) {
    _MoqJsonWindowProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqJsonWindowProducer({
    required MoqTrackProducer track,
    required MoqJsonWindowConfig config,
  }) : _ptr = rustCall(
         (status) => uniffi_moq_ffi_fn_constructor_moqjsonwindowproducer_new(
           FfiConverterMoqTrackProducer.lower(track),
           FfiConverterMoqJsonWindowConfig.lower(config),
           status,
         ),
         moqExceptionErrorHandler,
       ) {
    _MoqJsonWindowProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqJsonWindowProducer.lift(Pointer<Void> ptr) {
    return MoqJsonWindowProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqjsonwindowproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqJsonWindowProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqjsonwindowproducer(_ptr, status),
    );
  }

  void cut() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonwindowproducer_cut(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqTrackDemand demand() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqjsonwindowproducer_demand(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDemand.lift,
      moqExceptionErrorHandler,
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonwindowproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void pop({required int count, int? timestampUs = null}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonwindowproducer_pop(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(count),
        FfiConverterOptionalUInt64.lower(timestampUs),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void push({required String value, int? timestampUs = null}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqjsonwindowproducer_push(
        uniffiClonePointer(),
        FfiConverterString.lower(value),
        FfiConverterOptionalUInt64.lower(timestampUs),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqJsonWindowProducer {
  static MoqJsonWindowProducer lift(Pointer<Void> ptr) {
    return MoqJsonWindowProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqJsonWindowProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqJsonWindowProducer value) {
    return 8;
  }

  static LiftRetVal<MoqJsonWindowProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqJsonWindowProducer.lift(pointer), 8);
  }

  static int write(MoqJsonWindowProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaCatalogConsumerInterface {
  void cancel();
  UniffiRustFuture<MoqCatalog?> next();
}

final _MoqMediaCatalogConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqmediacatalogconsumer(ptr, status),
  );
});

class MoqMediaCatalogConsumer implements MoqMediaCatalogConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqMediaCatalogConsumer._(this._ptr) {
    _MoqMediaCatalogConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  static UniffiRustFuture<MoqMediaCatalogConsumer> subscribe({
    required MoqBroadcastConsumer broadcast,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_constructor_moqmediacatalogconsumer_subscribe(
        FfiConverterMoqBroadcastConsumer.lower(broadcast),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (int handle) =>
          MoqMediaCatalogConsumer._(Pointer<Void>.fromAddress(handle)),
      moqExceptionErrorHandler,
    );
  }

  factory MoqMediaCatalogConsumer.lift(Pointer<Void> ptr) {
    return MoqMediaCatalogConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediacatalogconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqMediaCatalogConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqmediacatalogconsumer(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediacatalogconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  UniffiRustFuture<MoqCatalog?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqmediacatalogconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqCatalog.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqMediaCatalogConsumer {
  static MoqMediaCatalogConsumer lift(Pointer<Void> ptr) {
    return MoqMediaCatalogConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaCatalogConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaCatalogConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaCatalogConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaCatalogConsumer.lift(pointer), 8);
  }

  static int write(MoqMediaCatalogConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaCatalogProducerInterface {
  void finish();
  int now();
  void removeSection({required String name});
  void setSection({required String name, required String json});
  void setVideoProperties({required MoqVideoProperties properties});
}

final _MoqMediaCatalogProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqmediacatalogproducer(ptr, status),
  );
});

class MoqMediaCatalogProducer implements MoqMediaCatalogProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqMediaCatalogProducer._(this._ptr) {
    _MoqMediaCatalogProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqMediaCatalogProducer({required MoqBroadcastProducer broadcast})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqmediacatalogproducer_new(
          FfiConverterMoqBroadcastProducer.lower(broadcast),
          status,
        ),
        moqExceptionErrorHandler,
      ) {
    _MoqMediaCatalogProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqMediaCatalogProducer.lift(Pointer<Void> ptr) {
    return MoqMediaCatalogProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediacatalogproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqMediaCatalogProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqmediacatalogproducer(_ptr, status),
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediacatalogproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  int now() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqmediacatalogproducer_now(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      moqExceptionErrorHandler,
    );
  }

  void removeSection({required String name}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediacatalogproducer_remove_section(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void setSection({required String name, required String json}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediacatalogproducer_set_section(
        uniffiClonePointer(),
        FfiConverterString.lower(name),
        FfiConverterString.lower(json),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void setVideoProperties({required MoqVideoProperties properties}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediacatalogproducer_set_video_properties(
        uniffiClonePointer(),
        FfiConverterMoqVideoProperties.lower(properties),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqMediaCatalogProducer {
  static MoqMediaCatalogProducer lift(Pointer<Void> ptr) {
    return MoqMediaCatalogProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaCatalogProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaCatalogProducer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaCatalogProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaCatalogProducer.lift(pointer), 8);
  }

  static int write(MoqMediaCatalogProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaContainerProducerInterface {
  void cut();
  void finish();
  void seek({required int sequence});
  void write({required Uint8List payload});
}

final _MoqMediaContainerProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqmediacontainerproducer(ptr, status),
  );
});

class MoqMediaContainerProducer implements MoqMediaContainerProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqMediaContainerProducer._(this._ptr) {
    _MoqMediaContainerProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqMediaContainerProducer({
    required MoqBroadcastProducer broadcast,
    required MoqContainerInit init,
  }) : _ptr = rustCall(
         (status) =>
             uniffi_moq_ffi_fn_constructor_moqmediacontainerproducer_new(
               FfiConverterMoqBroadcastProducer.lower(broadcast),
               FfiConverterMoqContainerInit.lower(init),
               status,
             ),
         moqExceptionErrorHandler,
       ) {
    _MoqMediaContainerProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqMediaContainerProducer.lift(Pointer<Void> ptr) {
    return MoqMediaContainerProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) =>
          uniffi_moq_ffi_fn_clone_moqmediacontainerproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqMediaContainerProducerFinalizer.detach(this);
    rustCall(
      (status) =>
          uniffi_moq_ffi_fn_free_moqmediacontainerproducer(_ptr, status),
    );
  }

  void cut() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediacontainerproducer_cut(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediacontainerproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void seek({required int sequence}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediacontainerproducer_seek(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(sequence),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void write({required Uint8List payload}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediacontainerproducer_write(
        uniffiClonePointer(),
        FfiConverterUint8List.lower(payload),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqMediaContainerProducer {
  static MoqMediaContainerProducer lift(Pointer<Void> ptr) {
    return MoqMediaContainerProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaContainerProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaContainerProducer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaContainerProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaContainerProducer.lift(pointer), 8);
  }

  static int write(MoqMediaContainerProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaGroupConsumerInterface {
  void cancel();
  UniffiRustFuture<MoqMediaFrame?> next();
  int sequence();
}

final _MoqMediaGroupConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqmediagroupconsumer(ptr, status),
  );
});

class MoqMediaGroupConsumer implements MoqMediaGroupConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqMediaGroupConsumer._(this._ptr) {
    _MoqMediaGroupConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqMediaGroupConsumer({
    required MoqGroupConsumer group,
    required MoqContainer container,
  }) : _ptr = rustCall(
         (status) => uniffi_moq_ffi_fn_constructor_moqmediagroupconsumer_new(
           FfiConverterMoqGroupConsumer.lower(group),
           FfiConverterMoqContainer.lower(container),
           status,
         ),
         moqExceptionErrorHandler,
       ) {
    _MoqMediaGroupConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  static UniffiRustFuture<MoqMediaGroupConsumer> fetch({
    required MoqBroadcastConsumer broadcast,
    required MoqMediaGroupConfig config,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_constructor_moqmediagroupconsumer_fetch(
        FfiConverterMoqBroadcastConsumer.lower(broadcast),
        FfiConverterMoqMediaGroupConfig.lower(config),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (int handle) =>
          MoqMediaGroupConsumer._(Pointer<Void>.fromAddress(handle)),
      moqExceptionErrorHandler,
    );
  }

  factory MoqMediaGroupConsumer.lift(Pointer<Void> ptr) {
    return MoqMediaGroupConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediagroupconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
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

  UniffiRustFuture<MoqMediaFrame?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqmediagroupconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
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

abstract class MoqMediaTimebaseInterface {}

final _MoqMediaTimebaseFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqmediatimebase(ptr, status));
});

class MoqMediaTimebase implements MoqMediaTimebaseInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqMediaTimebase._(this._ptr) {
    _MoqMediaTimebaseFinalizer.attach(this, _ptr, detach: this);
  }
  MoqMediaTimebase({required MoqBroadcastProducer broadcast})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqmediatimebase_new(
          FfiConverterMoqBroadcastProducer.lower(broadcast),
          status,
        ),
        moqExceptionErrorHandler,
      ) {
    _MoqMediaTimebaseFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqMediaTimebase.lift(Pointer<Void> ptr) {
    return MoqMediaTimebase._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediatimebase(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqMediaTimebaseFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqmediatimebase(_ptr, status));
  }
}

class FfiConverterMoqMediaTimebase {
  static MoqMediaTimebase lift(Pointer<Void> ptr) {
    return MoqMediaTimebase.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaTimebase value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaTimebase value) {
    return 8;
  }

  static LiftRetVal<MoqMediaTimebase> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaTimebase.lift(pointer), 8);
  }

  static int write(MoqMediaTimebase value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaTrackConsumerInterface {
  void cancel();
  UniffiRustFuture<MoqMediaRead?> next();
}

final _MoqMediaTrackConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqmediatrackconsumer(ptr, status),
  );
});

class MoqMediaTrackConsumer implements MoqMediaTrackConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqMediaTrackConsumer._(this._ptr) {
    _MoqMediaTrackConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  static UniffiRustFuture<MoqMediaTrackConsumer> subscribe({
    required MoqBroadcastConsumer broadcast,
    required MoqMediaTrackConfig config,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_constructor_moqmediatrackconsumer_subscribe(
        FfiConverterMoqBroadcastConsumer.lower(broadcast),
        FfiConverterMoqMediaTrackConfig.lower(config),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (int handle) =>
          MoqMediaTrackConsumer._(Pointer<Void>.fromAddress(handle)),
      moqExceptionErrorHandler,
    );
  }

  factory MoqMediaTrackConsumer.lift(Pointer<Void> ptr) {
    return MoqMediaTrackConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediatrackconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqMediaTrackConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqmediatrackconsumer(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediatrackconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  UniffiRustFuture<MoqMediaRead?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqmediatrackconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqMediaRead.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqMediaTrackConsumer {
  static MoqMediaTrackConsumer lift(Pointer<Void> ptr) {
    return MoqMediaTrackConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaTrackConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaTrackConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaTrackConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaTrackConsumer.lift(pointer), 8);
  }

  static int write(MoqMediaTrackConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaTrackProducerInterface {
  void cut();
  MoqTrackDemand demand();
  void discontinuity();
  void finish();
  void seek({required int sequence});
  void writeFrame({required MoqFrame frame});
}

final _MoqMediaTrackProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqmediatrackproducer(ptr, status),
  );
});

class MoqMediaTrackProducer implements MoqMediaTrackProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqMediaTrackProducer._(this._ptr) {
    _MoqMediaTrackProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqMediaTrackProducer.audio({
    required MoqBroadcastProducer broadcast,
    required MoqMediaTarget target,
    required MoqAudioInit init,
  }) : _ptr = rustCall(
         (status) => uniffi_moq_ffi_fn_constructor_moqmediatrackproducer_audio(
           FfiConverterMoqBroadcastProducer.lower(broadcast),
           FfiConverterMoqMediaTarget.lower(target),
           FfiConverterMoqAudioInit.lower(init),
           status,
         ),
         moqExceptionErrorHandler,
       ) {
    _MoqMediaTrackProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqMediaTrackProducer.video({
    required MoqBroadcastProducer broadcast,
    required MoqMediaTarget target,
    required MoqVideoInit init,
  }) : _ptr = rustCall(
         (status) => uniffi_moq_ffi_fn_constructor_moqmediatrackproducer_video(
           FfiConverterMoqBroadcastProducer.lower(broadcast),
           FfiConverterMoqMediaTarget.lower(target),
           FfiConverterMoqVideoInit.lower(init),
           status,
         ),
         moqExceptionErrorHandler,
       ) {
    _MoqMediaTrackProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqMediaTrackProducer.lift(Pointer<Void> ptr) {
    return MoqMediaTrackProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqmediatrackproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqMediaTrackProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqmediatrackproducer(_ptr, status),
    );
  }

  void cut() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediatrackproducer_cut(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqTrackDemand demand() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqmediatrackproducer_demand(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDemand.lift,
      moqExceptionErrorHandler,
    );
  }

  void discontinuity() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediatrackproducer_discontinuity(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediatrackproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void seek({required int sequence}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediatrackproducer_seek(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(sequence),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void writeFrame({required MoqFrame frame}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediatrackproducer_write_frame(
        uniffiClonePointer(),
        FfiConverterMoqFrame.lower(frame),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqMediaTrackProducer {
  static MoqMediaTrackProducer lift(Pointer<Void> ptr) {
    return MoqMediaTrackProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaTrackProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaTrackProducer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaTrackProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaTrackProducer.lift(pointer), 8);
  }

  static int write(MoqMediaTrackProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqMediaTrackStreamProducerInterface {
  MoqTrackDemand demand();
  void finish();
  void write({required Uint8List payload});
}

final _MoqMediaTrackStreamProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqmediatrackstreamproducer(ptr, status),
  );
});

class MoqMediaTrackStreamProducer
    implements MoqMediaTrackStreamProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqMediaTrackStreamProducer._(this._ptr) {
    _MoqMediaTrackStreamProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqMediaTrackStreamProducer.video({
    required MoqBroadcastProducer broadcast,
    required MoqMediaTarget target,
    required MoqVideoInit init,
  }) : _ptr = rustCall(
         (status) =>
             uniffi_moq_ffi_fn_constructor_moqmediatrackstreamproducer_video(
               FfiConverterMoqBroadcastProducer.lower(broadcast),
               FfiConverterMoqMediaTarget.lower(target),
               FfiConverterMoqVideoInit.lower(init),
               status,
             ),
         moqExceptionErrorHandler,
       ) {
    _MoqMediaTrackStreamProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqMediaTrackStreamProducer.lift(Pointer<Void> ptr) {
    return MoqMediaTrackStreamProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) =>
          uniffi_moq_ffi_fn_clone_moqmediatrackstreamproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqMediaTrackStreamProducerFinalizer.detach(this);
    rustCall(
      (status) =>
          uniffi_moq_ffi_fn_free_moqmediatrackstreamproducer(_ptr, status),
    );
  }

  MoqTrackDemand demand() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqmediatrackstreamproducer_demand(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDemand.lift,
      moqExceptionErrorHandler,
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediatrackstreamproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void write({required Uint8List payload}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqmediatrackstreamproducer_write(
        uniffiClonePointer(),
        FfiConverterUint8List.lower(payload),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqMediaTrackStreamProducer {
  static MoqMediaTrackStreamProducer lift(Pointer<Void> ptr) {
    return MoqMediaTrackStreamProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqMediaTrackStreamProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqMediaTrackStreamProducer value) {
    return 8;
  }

  static LiftRetVal<MoqMediaTrackStreamProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqMediaTrackStreamProducer.lift(pointer), 8);
  }

  static int write(MoqMediaTrackStreamProducer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAnnounceConsumerInterface {
  void cancel();
  UniffiRustFuture<MoqAnnounceEvent?> next();
}

final _MoqAnnounceConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqannounceconsumer(ptr, status));
});

class MoqAnnounceConsumer implements MoqAnnounceConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqAnnounceConsumer._(this._ptr) {
    _MoqAnnounceConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAnnounceConsumer.lift(Pointer<Void> ptr) {
    return MoqAnnounceConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqannounceconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqAnnounceConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqannounceconsumer(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqannounceconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  UniffiRustFuture<MoqAnnounceEvent?> next() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqannounceconsumer_next(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqAnnounceEvent.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqAnnounceConsumer {
  static MoqAnnounceConsumer lift(Pointer<Void> ptr) {
    return MoqAnnounceConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqAnnounceConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqAnnounceConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqAnnounceConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqAnnounceConsumer.lift(pointer), 8);
  }

  static int write(MoqAnnounceConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqAnnouncedBroadcastInterface {
  UniffiRustFuture<MoqBroadcastConsumer> available();
  void cancel();
}

final _MoqAnnouncedBroadcastFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqannouncedbroadcast(ptr, status),
  );
});

class MoqAnnouncedBroadcast implements MoqAnnouncedBroadcastInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqAnnouncedBroadcast._(this._ptr) {
    _MoqAnnouncedBroadcastFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqAnnouncedBroadcast.lift(Pointer<Void> ptr) {
    return MoqAnnouncedBroadcast._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqannouncedbroadcast(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqAnnouncedBroadcastFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqannouncedbroadcast(_ptr, status),
    );
  }

  UniffiRustFuture<MoqBroadcastConsumer> available() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqannouncedbroadcast_available(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
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

abstract class MoqBroadcastPreparedInterface {
  MoqBroadcastProducer broadcast();
  MoqPublication publish();
}

final _MoqBroadcastPreparedFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqbroadcastprepared(ptr, status),
  );
});

class MoqBroadcastPrepared implements MoqBroadcastPreparedInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqBroadcastPrepared._(this._ptr) {
    _MoqBroadcastPreparedFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqBroadcastPrepared.lift(Pointer<Void> ptr) {
    return MoqBroadcastPrepared._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbroadcastprepared(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqBroadcastPreparedFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbroadcastprepared(_ptr, status),
    );
  }

  MoqBroadcastProducer broadcast() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastprepared_broadcast(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqBroadcastProducer.lift,
      null,
    );
  }

  MoqPublication publish() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastprepared_publish(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqPublication.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqBroadcastPrepared {
  static MoqBroadcastPrepared lift(Pointer<Void> ptr) {
    return MoqBroadcastPrepared.lift(ptr);
  }

  static Pointer<Void> lower(MoqBroadcastPrepared value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqBroadcastPrepared value) {
    return 8;
  }

  static LiftRetVal<MoqBroadcastPrepared> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqBroadcastPrepared.lift(pointer), 8);
  }

  static int write(MoqBroadcastPrepared value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqBroadcastRequestInterface {
  String path();
  MoqBroadcastPrepared prepare({required String epoch});
  void reject({required int errorCode});
}

final _MoqBroadcastRequestFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqbroadcastrequest(ptr, status));
});

class MoqBroadcastRequest implements MoqBroadcastRequestInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqBroadcastRequest._(this._ptr) {
    _MoqBroadcastRequestFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqBroadcastRequest.lift(Pointer<Void> ptr) {
    return MoqBroadcastRequest._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbroadcastrequest(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqBroadcastRequestFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbroadcastrequest(_ptr, status),
    );
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

  MoqBroadcastPrepared prepare({required String epoch}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastrequest_prepare(
        uniffiClonePointer(),
        FfiConverterString.lower(epoch),
        status,
      ),
      FfiConverterMoqBroadcastPrepared.lift,
      moqExceptionErrorHandler,
    );
  }

  void reject({required int errorCode}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastrequest_reject(
        uniffiClonePointer(),
        FfiConverterUInt16.lower(errorCode),
        status,
      );
    }, moqExceptionErrorHandler);
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
  MoqAnnounceConsumer announced({required MoqAnnounceConfig config});
  MoqAnnouncedBroadcast announcedBroadcast({required String path});
  UniffiRustFuture<MoqBroadcastConsumer> requestBroadcast({
    required String path,
  });
}

final _MoqOriginConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqoriginconsumer(ptr, status));
});

class MoqOriginConsumer implements MoqOriginConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqOriginConsumer._(this._ptr) {
    _MoqOriginConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqOriginConsumer.lift(Pointer<Void> ptr) {
    return MoqOriginConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqoriginconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqOriginConsumerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqoriginconsumer(_ptr, status),
    );
  }

  MoqAnnounceConsumer announced({required MoqAnnounceConfig config}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqoriginconsumer_announced(
        uniffiClonePointer(),
        FfiConverterMoqAnnounceConfig.lower(config),
        status,
      ),
      FfiConverterMoqAnnounceConsumer.lift,
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

  UniffiRustFuture<MoqBroadcastConsumer> requestBroadcast({
    required String path,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqoriginconsumer_request_broadcast(
        uniffiClonePointer(),
        FfiConverterString.lower(path),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
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

abstract class MoqOriginProducerInterface {
  MoqOriginConsumer consume();
  MoqPublication createBroadcast({required String path});
  MoqOriginRequests requests({required String prefix});
}

final _MoqOriginProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqoriginproducer(ptr, status));
});

class MoqOriginProducer implements MoqOriginProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqOriginProducer._(this._ptr) {
    _MoqOriginProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqOriginProducer({required MoqOriginConfig config})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqoriginproducer_new(
          FfiConverterMoqOriginConfig.lower(config),
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
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqoriginproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqOriginProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqoriginproducer(_ptr, status),
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

  MoqPublication createBroadcast({required String path}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqoriginproducer_create_broadcast(
        uniffiClonePointer(),
        FfiConverterString.lower(path),
        status,
      ),
      FfiConverterMoqPublication.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqOriginRequests requests({required String prefix}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqoriginproducer_requests(
        uniffiClonePointer(),
        FfiConverterString.lower(prefix),
        status,
      ),
      FfiConverterMoqOriginRequests.lift,
      moqExceptionErrorHandler,
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

abstract class MoqOriginRequestsInterface {
  void cancel();
  UniffiRustFuture<MoqBroadcastRequest> requestedBroadcast();
}

final _MoqOriginRequestsFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqoriginrequests(ptr, status));
});

class MoqOriginRequests implements MoqOriginRequestsInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqOriginRequests._(this._ptr) {
    _MoqOriginRequestsFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqOriginRequests.lift(Pointer<Void> ptr) {
    return MoqOriginRequests._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqoriginrequests(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqOriginRequestsFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqoriginrequests(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqoriginrequests_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  UniffiRustFuture<MoqBroadcastRequest> requestedBroadcast() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqoriginrequests_requested_broadcast(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) =>
          FfiConverterMoqBroadcastRequest.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqOriginRequests {
  static MoqOriginRequests lift(Pointer<Void> ptr) {
    return MoqOriginRequests.lift(ptr);
  }

  static Pointer<Void> lower(MoqOriginRequests value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqOriginRequests value) {
    return 8;
  }

  static LiftRetVal<MoqOriginRequests> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqOriginRequests.lift(pointer), 8);
  }

  static int write(MoqOriginRequests value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqPublicationInterface {
  void announce({required MoqAnnounceRoute route});
  MoqBroadcastProducer broadcast();
  void unannounce();
}

final _MoqPublicationFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqpublication(ptr, status));
});

class MoqPublication implements MoqPublicationInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqPublication._(this._ptr) {
    _MoqPublicationFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqPublication.lift(Pointer<Void> ptr) {
    return MoqPublication._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqpublication(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqPublicationFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqpublication(_ptr, status));
  }

  void announce({required MoqAnnounceRoute route}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqpublication_announce(
        uniffiClonePointer(),
        FfiConverterMoqAnnounceRoute.lower(route),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqBroadcastProducer broadcast() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqpublication_broadcast(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqBroadcastProducer.lift,
      null,
    );
  }

  void unannounce() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqpublication_unannounce(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqPublication {
  static MoqPublication lift(Pointer<Void> ptr) {
    return MoqPublication.lift(ptr);
  }

  static Pointer<Void> lower(MoqPublication value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqPublication value) {
    return 8;
  }

  static LiftRetVal<MoqPublication> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqPublication.lift(pointer), 8);
  }

  static int write(MoqPublication value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqBroadcastProducerInterface {
  void close();
  MoqBroadcastConsumer consume();
  MoqTrackProducer publishTrack({
    required String name,
    required MoqTrackInfo? info,
  });
  MoqBroadcastRequests requests();
}

final _MoqBroadcastProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqbroadcastproducer(ptr, status),
  );
});

class MoqBroadcastProducer implements MoqBroadcastProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
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
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbroadcastproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqBroadcastProducerFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbroadcastproducer(_ptr, status),
    );
  }

  void close() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastproducer_close(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
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

  MoqBroadcastRequests requests() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqbroadcastproducer_requests(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqBroadcastRequests.lift,
      moqExceptionErrorHandler,
    );
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

abstract class MoqBroadcastRequestsInterface {
  void cancel();
  UniffiRustFuture<MoqTrackRequest> requestedTrack();
}

final _MoqBroadcastRequestsFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqbroadcastrequests(ptr, status),
  );
});

class MoqBroadcastRequests implements MoqBroadcastRequestsInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqBroadcastRequests._(this._ptr) {
    _MoqBroadcastRequestsFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqBroadcastRequests.lift(Pointer<Void> ptr) {
    return MoqBroadcastRequests._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqbroadcastrequests(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqBroadcastRequestsFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqbroadcastrequests(_ptr, status),
    );
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqbroadcastrequests_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  UniffiRustFuture<MoqTrackRequest> requestedTrack() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqbroadcastrequests_requested_track(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqTrackRequest.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqBroadcastRequests {
  static MoqBroadcastRequests lift(Pointer<Void> ptr) {
    return MoqBroadcastRequests.lift(ptr);
  }

  static Pointer<Void> lower(MoqBroadcastRequests value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqBroadcastRequests value) {
    return 8;
  }

  static LiftRetVal<MoqBroadcastRequests> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqBroadcastRequests.lift(pointer), 8);
  }

  static int write(MoqBroadcastRequests value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqFillInterface {
  MoqGroupProducer accept({required int sequence});
  void dropGroups({required int start, required int end});
  int endGroup();
  bool isUsed();
  bool isWanted({required int sequence});
  Duration? maxDelay();
  int? next();
  MoqOrder order();
  int priority();
  int startFrame();
  int startGroup();
  UniffiRustFuture<void> unused();
  UniffiRustFuture<void> unwanted({required int sequence});
}

final _MoqFillFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqfill(ptr, status));
});

class MoqFill implements MoqFillInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqFill._(this._ptr) {
    _MoqFillFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqFill.lift(Pointer<Void> ptr) {
    return MoqFill._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall((status) => uniffi_moq_ffi_fn_clone_moqfill(_ptr, status));
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqFillFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqfill(_ptr, status));
  }

  MoqGroupProducer accept({required int sequence}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqfill_accept(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(sequence),
        status,
      ),
      FfiConverterMoqGroupProducer.lift,
      moqExceptionErrorHandler,
    );
  }

  void dropGroups({required int start, required int end}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqfill_drop_groups(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(start),
        FfiConverterUInt64.lower(end),
        status,
      );
    }, null);
  }

  int endGroup() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqfill_end_group(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      null,
    );
  }

  bool isUsed() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqfill_is_used(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterBool.lift,
      null,
    );
  }

  bool isWanted({required int sequence}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqfill_is_wanted(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(sequence),
        status,
      ),
      FfiConverterBool.lift,
      null,
    );
  }

  Duration? maxDelay() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqfill_max_delay(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalDuration.lift,
      null,
    );
  }

  int? next() {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqfill_next(uniffiClonePointer(), status),
      FfiConverterOptionalUInt64.lift,
      null,
    );
  }

  MoqOrder order() {
    return rustCallWithLifter(
      (status) =>
          uniffi_moq_ffi_fn_method_moqfill_order(uniffiClonePointer(), status),
      FfiConverterMoqOrder.lift,
      null,
    );
  }

  int priority() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqfill_priority(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterInt8.lift,
      null,
    );
  }

  int startFrame() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqfill_start_frame(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      null,
    );
  }

  int startGroup() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqfill_start_group(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      null,
    );
  }

  UniffiRustFuture<void> unused() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqfill_unused(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_cancel_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  UniffiRustFuture<void> unwanted({required int sequence}) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqfill_unwanted(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(sequence),
      ),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_cancel_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqFill {
  static MoqFill lift(Pointer<Void> ptr) {
    return MoqFill.lift(ptr);
  }

  static Pointer<Void> lower(MoqFill value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqFill value) {
    return 8;
  }

  static LiftRetVal<MoqFill> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqFill.lift(pointer), 8);
  }

  static int write(MoqFill value, Uint8List buf) {
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
  bool _isDisposed = false;
  MoqGroupProducer._(this._ptr) {
    _MoqGroupProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqGroupProducer.lift(Pointer<Void> ptr) {
    return MoqGroupProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqgroupproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
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

abstract class MoqTrackProducerInterface {
  void abort({required int errorCode});
  int appendDatagram({required MoqFrame frame});
  MoqGroupProducer appendGroup();
  MoqTrackConsumer consume({required MoqSubscription? subscription});
  MoqGroupProducer createGroup({required int sequence});
  MoqTrackDemand demand();
  void finish();
  void finishAt({required int finalSequence});
  MoqTrackRequests requests();
  void writeFrame({required MoqFrame frame});
}

final _MoqTrackProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackproducer(ptr, status));
});

class MoqTrackProducer implements MoqTrackProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqTrackProducer._(this._ptr) {
    _MoqTrackProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTrackProducer.lift(Pointer<Void> ptr) {
    return MoqTrackProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtrackproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
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

  MoqTrackDemand demand() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackproducer_demand(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDemand.lift,
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

  MoqTrackRequests requests() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackproducer_requests(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackRequests.lift,
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
  MoqTrackDemand demand();
  String name();
  MoqTrackRequests requests();
}

final _MoqTrackRequestFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackrequest(ptr, status));
});

class MoqTrackRequest implements MoqTrackRequestInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqTrackRequest._(this._ptr) {
    _MoqTrackRequestFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTrackRequest.lift(Pointer<Void> ptr) {
    return MoqTrackRequest._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtrackrequest(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
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

  MoqTrackDemand demand() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackrequest_demand(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDemand.lift,
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

  MoqTrackRequests requests() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqtrackrequest_requests(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackRequests.lift,
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

abstract class MoqTrackRequestsInterface {
  void cancel();
  UniffiRustFuture<MoqFill> requestedFill();
}

final _MoqTrackRequestsFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackrequests(ptr, status));
});

class MoqTrackRequests implements MoqTrackRequestsInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqTrackRequests._(this._ptr) {
    _MoqTrackRequestsFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTrackRequests.lift(Pointer<Void> ptr) {
    return MoqTrackRequests._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtrackrequests(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqTrackRequestsFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqtrackrequests(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtrackrequests_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  UniffiRustFuture<MoqFill> requestedFill() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtrackrequests_requested_fill(
        uniffiClonePointer(),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqFill.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqTrackRequests {
  static MoqTrackRequests lift(Pointer<Void> ptr) {
    return MoqTrackRequests.lift(ptr);
  }

  static Pointer<Void> lower(MoqTrackRequests value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqTrackRequests value) {
    return 8;
  }

  static LiftRetVal<MoqTrackRequests> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqTrackRequests.lift(pointer), 8);
  }

  static int write(MoqTrackRequests value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqRequestInterface {
  UniffiRustFuture<MoqSession> accept({
    MoqOriginProducer? publish = null,
    MoqOriginProducer? consume = null,
  });
  void cancel();
  String path();
  String? peerImplementation();
  String? query();
  UniffiRustFuture<void> reject({required int code});
  MoqTransport transport();
  String? url();
}

final _MoqRequestFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqrequest(ptr, status));
});

class MoqRequest implements MoqRequestInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqRequest._(this._ptr) {
    _MoqRequestFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqRequest.lift(Pointer<Void> ptr) {
    return MoqRequest._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqrequest(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqRequestFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqrequest(_ptr, status));
  }

  UniffiRustFuture<MoqSession> accept({
    MoqOriginProducer? publish = null,
    MoqOriginProducer? consume = null,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqrequest_accept(
        uniffiClonePointer(),
        FfiConverterOptionalMoqOriginProducer.lower(publish),
        FfiConverterOptionalMoqOriginProducer.lower(consume),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
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

  String? peerImplementation() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqrequest_peer_implementation(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalString.lift,
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

  UniffiRustFuture<void> reject({required int code}) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqrequest_reject(
        uniffiClonePointer(),
        FfiConverterUInt16.lower(code),
      ),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_cancel_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  MoqTransport transport() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqrequest_transport(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTransport.lift,
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
  UniffiRustFuture<MoqRequest?> accept();
  void cancel();
  List<String> certFingerprints();
  UniffiRustFuture<String> listen();
}

final _MoqServerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqserver(ptr, status));
});

class MoqServer implements MoqServerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqServer._(this._ptr) {
    _MoqServerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqServer({required MoqServerConfig config})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqserver_new(
          FfiConverterMoqServerConfig.lower(config),
          status,
        ),
        moqExceptionErrorHandler,
      ) {
    _MoqServerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqServer.lift(Pointer<Void> ptr) {
    return MoqServer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqserver(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqServerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqserver(_ptr, status));
  }

  UniffiRustFuture<MoqRequest?> accept() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqserver_accept(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
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

  UniffiRustFuture<String> listen() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqserver_listen(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterString.lift,
      moqExceptionErrorHandler,
    );
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
  UniffiRustFuture<MoqSession> connect({required String url});
}

final _MoqClientFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqclient(ptr, status));
});

class MoqClient implements MoqClientInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqClient._(this._ptr) {
    _MoqClientFinalizer.attach(this, _ptr, detach: this);
  }
  MoqClient({required MoqClientConfig config})
    : _ptr = rustCall(
        (status) => uniffi_moq_ffi_fn_constructor_moqclient_new(
          FfiConverterMoqClientConfig.lower(config),
          status,
        ),
        moqExceptionErrorHandler,
      ) {
    _MoqClientFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqClient.lift(Pointer<Void> ptr) {
    return MoqClient._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqclient(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqClientFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqclient(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqclient_cancel(uniffiClonePointer(), status);
    }, null);
  }

  UniffiRustFuture<MoqSession> connect({required String url}) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqclient_connect(
        uniffiClonePointer(),
        FfiConverterString.lower(url),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (ptr) => FfiConverterMoqSession.lift(Pointer<Void>.fromAddress(ptr)),
      moqExceptionErrorHandler,
    );
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
  MoqAllocator allocator();
  void cancel({required int code});
  UniffiRustFuture<void> closed();
  MoqOriginConsumer consume();
  MoqGrantConsumer grants();
  MoqOriginProducer publish();
  MoqBandwidthConsumer? recvBandwidth();
  void shutdown();
  MoqConnectionState state();
  UniffiRustFuture<MoqConnectionState> stateChanged({required int after});
  MoqConnectionStats stats();
  MoqTokens? tokens();
}

final _MoqSessionFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqsession(ptr, status));
});

class MoqSession implements MoqSessionInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqSession._(this._ptr) {
    _MoqSessionFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqSession.lift(Pointer<Void> ptr) {
    return MoqSession._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqsession(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqSessionFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqsession(_ptr, status));
  }

  MoqAllocator allocator() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_allocator(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqAllocator.lift,
      null,
    );
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

  UniffiRustFuture<void> closed() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqsession_closed(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_cancel_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  MoqOriginConsumer consume() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_consume(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqOriginConsumer.lift,
      null,
    );
  }

  MoqGrantConsumer grants() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_grants(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqGrantConsumer.lift,
      null,
    );
  }

  MoqOriginProducer publish() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_publish(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqOriginProducer.lift,
      null,
    );
  }

  MoqBandwidthConsumer? recvBandwidth() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_recv_bandwidth(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalMoqBandwidthConsumer.lift,
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

  MoqConnectionState state() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_state(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqConnectionState.lift,
      null,
    );
  }

  UniffiRustFuture<MoqConnectionState> stateChanged({required int after}) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqsession_state_changed(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(after),
      ),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterMoqConnectionState.lift,
      moqExceptionErrorHandler,
    );
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

  MoqTokens? tokens() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqsession_tokens(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalMoqTokens.lift,
      null,
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

abstract class MoqTokensInterface {
  UniffiRustFuture<void> answered();
  void set_({required List<String> tokens});
}

final _MoqTokensFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqtokens(ptr, status));
});

class MoqTokens implements MoqTokensInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqTokens._(this._ptr) {
    _MoqTokensFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqTokens.lift(Pointer<Void> ptr) {
    return MoqTokens._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqtokens(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqTokensFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqtokens(_ptr, status));
  }

  UniffiRustFuture<void> answered() {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_method_moqtokens_answered(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_void,
      ffi_moq_ffi_rust_future_complete_void,
      ffi_moq_ffi_rust_future_cancel_void,
      ffi_moq_ffi_rust_future_free_void,
      (_) {},
      moqExceptionErrorHandler,
    );
  }

  void set_({required List<String> tokens}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqtokens_set(
        uniffiClonePointer(),
        FfiConverterSequenceString.lower(tokens),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqTokens {
  static MoqTokens lift(Pointer<Void> ptr) {
    return MoqTokens.lift(ptr);
  }

  static Pointer<Void> lower(MoqTokens value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqTokens value) {
    return 8;
  }

  static LiftRetVal<MoqTokens> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqTokens.lift(pointer), 8);
  }

  static int write(MoqTokens value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqVideoConsumerInterface {
  void cancel();
  UniffiRustFuture<MoqVideoRead?> next();
}

final _MoqVideoConsumerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqvideoconsumer(ptr, status));
});

class MoqVideoConsumer implements MoqVideoConsumerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqVideoConsumer._(this._ptr) {
    _MoqVideoConsumerFinalizer.attach(this, _ptr, detach: this);
  }
  static UniffiRustFuture<MoqVideoConsumer> subscribe({
    required MoqBroadcastConsumer broadcast,
    required String id,
    required MoqVideo catalogVideo,
    required MoqVideoDecoderOutput output,
  }) {
    return uniffiRustCallAsync(
      () => uniffi_moq_ffi_fn_constructor_moqvideoconsumer_subscribe(
        FfiConverterMoqBroadcastConsumer.lower(broadcast),
        FfiConverterString.lower(id),
        FfiConverterMoqVideo.lower(catalogVideo),
        FfiConverterMoqVideoDecoderOutput.lower(output),
      ),
      ffi_moq_ffi_rust_future_poll_u64,
      ffi_moq_ffi_rust_future_complete_u64,
      ffi_moq_ffi_rust_future_cancel_u64,
      ffi_moq_ffi_rust_future_free_u64,
      (int handle) => MoqVideoConsumer._(Pointer<Void>.fromAddress(handle)),
      moqExceptionErrorHandler,
    );
  }

  factory MoqVideoConsumer.lift(Pointer<Void> ptr) {
    return MoqVideoConsumer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqvideoconsumer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqVideoConsumerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqvideoconsumer(_ptr, status));
  }

  void cancel() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqvideoconsumer_cancel(
        uniffiClonePointer(),
        status,
      );
    }, null);
  }

  UniffiRustFuture<MoqVideoRead?> next() {
    return uniffiRustCallAsync(
      () =>
          uniffi_moq_ffi_fn_method_moqvideoconsumer_next(uniffiClonePointer()),
      ffi_moq_ffi_rust_future_poll_rust_buffer,
      ffi_moq_ffi_rust_future_complete_rust_buffer,
      ffi_moq_ffi_rust_future_cancel_rust_buffer,
      ffi_moq_ffi_rust_future_free_rust_buffer,
      FfiConverterOptionalMoqVideoRead.lift,
      moqExceptionErrorHandler,
    );
  }
}

class FfiConverterMoqVideoConsumer {
  static MoqVideoConsumer lift(Pointer<Void> ptr) {
    return MoqVideoConsumer.lift(ptr);
  }

  static Pointer<Void> lower(MoqVideoConsumer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqVideoConsumer value) {
    return 8;
  }

  static LiftRetVal<MoqVideoConsumer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqVideoConsumer.lift(pointer), 8);
  }

  static int write(MoqVideoConsumer value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqVideoDecodedFrameInterface {
  int height();
  Uint8List pixels({required MoqVideoPixelFormat format});
  MoqVideoSurface? surface();
  int timestampUs();
  int width();
}

final _MoqVideoDecodedFrameFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall(
    (status) => uniffi_moq_ffi_fn_free_moqvideodecodedframe(ptr, status),
  );
});

class MoqVideoDecodedFrame implements MoqVideoDecodedFrameInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqVideoDecodedFrame._(this._ptr) {
    _MoqVideoDecodedFrameFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqVideoDecodedFrame.lift(Pointer<Void> ptr) {
    return MoqVideoDecodedFrame._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqvideodecodedframe(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqVideoDecodedFrameFinalizer.detach(this);
    rustCall(
      (status) => uniffi_moq_ffi_fn_free_moqvideodecodedframe(_ptr, status),
    );
  }

  int height() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideodecodedframe_height(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt32.lift,
      null,
    );
  }

  Uint8List pixels({required MoqVideoPixelFormat format}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideodecodedframe_pixels(
        uniffiClonePointer(),
        FfiConverterMoqVideoPixelFormat.lower(format),
        status,
      ),
      FfiConverterUint8List.lift,
      moqExceptionErrorHandler,
    );
  }

  MoqVideoSurface? surface() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideodecodedframe_surface(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalMoqVideoSurface.lift,
      null,
    );
  }

  int timestampUs() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideodecodedframe_timestamp_us(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt64.lift,
      null,
    );
  }

  int width() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideodecodedframe_width(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterUInt32.lift,
      null,
    );
  }
}

class FfiConverterMoqVideoDecodedFrame {
  static MoqVideoDecodedFrame lift(Pointer<Void> ptr) {
    return MoqVideoDecodedFrame.lift(ptr);
  }

  static Pointer<Void> lower(MoqVideoDecodedFrame value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqVideoDecodedFrame value) {
    return 8;
  }

  static LiftRetVal<MoqVideoDecodedFrame> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqVideoDecodedFrame.lift(pointer), 8);
  }

  static int write(MoqVideoDecodedFrame value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqVideoEncoderInterface {
  void cut();
  List<MoqVideoPacket> encode({required MoqVideoFrame frame});
  List<MoqVideoPacket> finish();
  List<MoqVideoPacket> flush();
  void setBitrate({required int bitrate});
}

final _MoqVideoEncoderFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqvideoencoder(ptr, status));
});

class MoqVideoEncoder implements MoqVideoEncoderInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqVideoEncoder._(this._ptr) {
    _MoqVideoEncoderFinalizer.attach(this, _ptr, detach: this);
  }
  MoqVideoEncoder({
    required MoqVideoEncoderInput input,
    required MoqVideoEncoderOutput output,
  }) : _ptr = rustCall(
         (status) => uniffi_moq_ffi_fn_constructor_moqvideoencoder_new(
           FfiConverterMoqVideoEncoderInput.lower(input),
           FfiConverterMoqVideoEncoderOutput.lower(output),
           status,
         ),
         moqExceptionErrorHandler,
       ) {
    _MoqVideoEncoderFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqVideoEncoder.lift(Pointer<Void> ptr) {
    return MoqVideoEncoder._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqvideoencoder(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqVideoEncoderFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqvideoencoder(_ptr, status));
  }

  void cut() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqvideoencoder_cut(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  List<MoqVideoPacket> encode({required MoqVideoFrame frame}) {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideoencoder_encode(
        uniffiClonePointer(),
        FfiConverterMoqVideoFrame.lower(frame),
        status,
      ),
      FfiConverterSequenceMoqVideoPacket.lift,
      moqExceptionErrorHandler,
    );
  }

  List<MoqVideoPacket> finish() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideoencoder_finish(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterSequenceMoqVideoPacket.lift,
      moqExceptionErrorHandler,
    );
  }

  List<MoqVideoPacket> flush() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideoencoder_flush(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterSequenceMoqVideoPacket.lift,
      moqExceptionErrorHandler,
    );
  }

  void setBitrate({required int bitrate}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqvideoencoder_set_bitrate(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(bitrate),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqVideoEncoder {
  static MoqVideoEncoder lift(Pointer<Void> ptr) {
    return MoqVideoEncoder.lift(ptr);
  }

  static Pointer<Void> lower(MoqVideoEncoder value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqVideoEncoder value) {
    return 8;
  }

  static LiftRetVal<MoqVideoEncoder> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqVideoEncoder.lift(pointer), 8);
  }

  static int write(MoqVideoEncoder value, Uint8List buf) {
    final handle = lower(value);
    buf.buffer.asByteData(buf.offsetInBytes).setInt64(0, handle.address);
    return 8;
  }
}

abstract class MoqVideoProducerInterface {
  void cut();
  MoqTrackDemand demand();
  void finish();
  MoqReservation? reservation();
  void setBitrate({required int bitrate});
  void write({required MoqVideoFrame frame});
}

final _MoqVideoProducerFinalizer = Finalizer<Pointer<Void>>((ptr) {
  rustCall((status) => uniffi_moq_ffi_fn_free_moqvideoproducer(ptr, status));
});

class MoqVideoProducer implements MoqVideoProducerInterface {
  late final Pointer<Void> _ptr;
  bool _isDisposed = false;
  MoqVideoProducer._(this._ptr) {
    _MoqVideoProducerFinalizer.attach(this, _ptr, detach: this);
  }
  MoqVideoProducer({
    required MoqMediaCatalogProducer catalog,
    required MoqVideoProducerOptions options,
  }) : _ptr = rustCall(
         (status) => uniffi_moq_ffi_fn_constructor_moqvideoproducer_new(
           FfiConverterMoqMediaCatalogProducer.lower(catalog),
           FfiConverterMoqVideoProducerOptions.lower(options),
           status,
         ),
         moqExceptionErrorHandler,
       ) {
    _MoqVideoProducerFinalizer.attach(this, _ptr, detach: this);
  }
  factory MoqVideoProducer.lift(Pointer<Void> ptr) {
    return MoqVideoProducer._(ptr);
  }
  Pointer<Void> uniffiClonePointer() {
    if (_isDisposed) {
      throw StateError("Object has already been disposed");
    }
    return rustCall(
      (status) => uniffi_moq_ffi_fn_clone_moqvideoproducer(_ptr, status),
    );
  }

  void dispose() {
    if (_isDisposed) {
      return;
    }
    _isDisposed = true;
    _MoqVideoProducerFinalizer.detach(this);
    rustCall((status) => uniffi_moq_ffi_fn_free_moqvideoproducer(_ptr, status));
  }

  void cut() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqvideoproducer_cut(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqTrackDemand demand() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideoproducer_demand(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterMoqTrackDemand.lift,
      moqExceptionErrorHandler,
    );
  }

  void finish() {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqvideoproducer_finish(
        uniffiClonePointer(),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  MoqReservation? reservation() {
    return rustCallWithLifter(
      (status) => uniffi_moq_ffi_fn_method_moqvideoproducer_reservation(
        uniffiClonePointer(),
        status,
      ),
      FfiConverterOptionalMoqReservation.lift,
      null,
    );
  }

  void setBitrate({required int bitrate}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqvideoproducer_set_bitrate(
        uniffiClonePointer(),
        FfiConverterUInt64.lower(bitrate),
        status,
      );
    }, moqExceptionErrorHandler);
  }

  void write({required MoqVideoFrame frame}) {
    return rustCall((status) {
      uniffi_moq_ffi_fn_method_moqvideoproducer_write(
        uniffiClonePointer(),
        FfiConverterMoqVideoFrame.lower(frame),
        status,
      );
    }, moqExceptionErrorHandler);
  }
}

class FfiConverterMoqVideoProducer {
  static MoqVideoProducer lift(Pointer<Void> ptr) {
    return MoqVideoProducer.lift(ptr);
  }

  static Pointer<Void> lower(MoqVideoProducer value) {
    return value.uniffiClonePointer();
  }

  static int allocationSize(MoqVideoProducer value) {
    return 8;
  }

  static LiftRetVal<MoqVideoProducer> read(Uint8List buf) {
    final handle = buf.buffer.asByteData(buf.offsetInBytes).getInt64(0);
    final pointer = Pointer<Void>.fromAddress(handle);
    return LiftRetVal(MoqVideoProducer.lift(pointer), 8);
  }

  static int write(MoqVideoProducer value, Uint8List buf) {
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

class FfiConverterDouble32 {
  static double lift(double value) => value;
  static LiftRetVal<double> read(Uint8List buf) {
    return LiftRetVal(
      buf.buffer.asByteData(buf.offsetInBytes).getFloat32(0),
      4,
    );
  }

  static double lower(double value) => value;
  static int allocationSize([double value = 0]) {
    return 4;
  }

  static int write(double value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setFloat32(0, value);
    return FfiConverterDouble32.allocationSize();
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

class FfiConverterDuration {
  static Duration lift(RustBuffer buf) {
    return FfiConverterDuration.read(buf.asUint8List()).value;
  }

  static RustBuffer lower(Duration value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }

  static LiftRetVal<Duration> read(Uint8List buf) {
    final bytes = buf.buffer.asByteData(buf.offsetInBytes, 12);
    final seconds = bytes.getUint64(0);
    final micros = (bytes.getUint32(8) ~/ 1000);
    return LiftRetVal(Duration(seconds: seconds, microseconds: micros), 12);
  }

  static int allocationSize([Duration value = const Duration()]) {
    return 12;
  }

  static int write(Duration value, Uint8List buf) {
    if (value.isNegative) {
      throw ArgumentError.value(
        value,
        "value",
        "Duration must not be negative",
      );
    }
    final bytes = buf.buffer.asByteData(buf.offsetInBytes, 12);
    bytes.setUint64(0, value.inSeconds);
    final ms = (value.inMicroseconds - (value.inSeconds * 1000000)) * 1000;
    bytes.setUint32(8, ms.toInt());
    return 12;
  }
}

class FfiConverterInt32 {
  static int lift(int value) => value;
  static LiftRetVal<int> read(Uint8List buf) {
    return LiftRetVal(buf.buffer.asByteData(buf.offsetInBytes).getInt32(0), 4);
  }

  static int lower(int value) {
    if (value < -2147483648 || value > 2147483647) {
      throw ArgumentError("Value out of range for i32: " + value.toString());
    }
    return value;
  }

  static int allocationSize([int value = 0]) {
    return 4;
  }

  static int write(int value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, lower(value));
    return 4;
  }
}

class FfiConverterInt8 {
  static int lift(int value) => value;
  static LiftRetVal<int> read(Uint8List buf) {
    return LiftRetVal(buf.buffer.asByteData(buf.offsetInBytes).getInt8(0), 1);
  }

  static int lower(int value) {
    if (value < -128 || value > 127) {
      throw ArgumentError("Value out of range for i8: " + value.toString());
    }
    return value;
  }

  static int allocationSize([int value = 0]) {
    return 1;
  }

  static int write(int value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt8(0, lower(value));
    return 1;
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
    final buf = Uint8List(FfiConverterOptionalBool.allocationSize(value));
    FfiConverterOptionalBool.write(value, buf);
    return toRustBuffer(buf);
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
    final buf = Uint8List(FfiConverterOptionalDouble64.allocationSize(value));
    FfiConverterOptionalDouble64.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalDuration {
  static Duration? lift(RustBuffer buf) {
    return FfiConverterOptionalDuration.read(buf.asUint8List()).value;
  }

  static LiftRetVal<Duration?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterDuration.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<Duration?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([Duration? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterDuration.allocationSize(value) + 1;
  }

  static RustBuffer lower(Duration? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(FfiConverterOptionalDuration.allocationSize(value));
    FfiConverterOptionalDuration.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(Duration? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterDuration.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqAllocator {
  static MoqAllocator? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqAllocator.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAllocator?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqAllocator.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqAllocator?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqAllocator? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqAllocator.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqAllocator? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqAllocator.allocationSize(value),
    );
    FfiConverterOptionalMoqAllocator.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAllocator? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqAllocator.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqAnchor {
  static MoqAnchor? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqAnchor.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAnchor?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqAnchor.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqAnchor?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqAnchor? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqAnchor.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqAnchor? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(FfiConverterOptionalMoqAnchor.allocationSize(value));
    FfiConverterOptionalMoqAnchor.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAnchor? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqAnchor.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqAnnounceEvent {
  static MoqAnnounceEvent? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqAnnounceEvent.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAnnounceEvent?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqAnnounceEvent.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqAnnounceEvent?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqAnnounceEvent? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqAnnounceEvent.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqAnnounceEvent? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqAnnounceEvent.allocationSize(value),
    );
    FfiConverterOptionalMoqAnnounceEvent.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAnnounceEvent? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqAnnounceEvent.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqAudioRead {
  static MoqAudioRead? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqAudioRead.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqAudioRead?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqAudioRead.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqAudioRead?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqAudioRead? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqAudioRead.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqAudioRead? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqAudioRead.allocationSize(value),
    );
    FfiConverterOptionalMoqAudioRead.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqAudioRead? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqAudioRead.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqBandwidthConsumer {
  static MoqBandwidthConsumer? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqBandwidthConsumer.read(
      buf.asUint8List(),
    ).value;
  }

  static LiftRetVal<MoqBandwidthConsumer?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqBandwidthConsumer.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqBandwidthConsumer?>(
      result.value,
      result.bytesRead + 1,
    );
  }

  static int allocationSize([MoqBandwidthConsumer? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqBandwidthConsumer.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqBandwidthConsumer? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqBandwidthConsumer.allocationSize(value),
    );
    FfiConverterOptionalMoqBandwidthConsumer.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqBandwidthConsumer? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqBandwidthConsumer.write(
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
    final buf = Uint8List(FfiConverterOptionalMoqCatalog.allocationSize(value));
    FfiConverterOptionalMoqCatalog.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalMoqCongestionControl {
  static MoqCongestionControl? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqCongestionControl.read(
      buf.asUint8List(),
    ).value;
  }

  static LiftRetVal<MoqCongestionControl?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqCongestionControl.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqCongestionControl?>(
      result.value,
      result.bytesRead + 1,
    );
  }

  static int allocationSize([MoqCongestionControl? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqCongestionControl.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqCongestionControl? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqCongestionControl.allocationSize(value),
    );
    FfiConverterOptionalMoqCongestionControl.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqCongestionControl? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqCongestionControl.write(
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
    final buf = Uint8List(
      FfiConverterOptionalMoqDatagram.allocationSize(value),
    );
    FfiConverterOptionalMoqDatagram.write(value, buf);
    return toRustBuffer(buf);
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
    final buf = Uint8List(
      FfiConverterOptionalMoqDimensions.allocationSize(value),
    );
    FfiConverterOptionalMoqDimensions.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalMoqException {
  static MoqException? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqException.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqException?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqException.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqException?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqException? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqException.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqException? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqException.allocationSize(value),
    );
    FfiConverterOptionalMoqException.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqException? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqException.write(
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
    final buf = Uint8List(
      FfiConverterOptionalMoqFetchGroupOptions.allocationSize(value),
    );
    FfiConverterOptionalMoqFetchGroupOptions.write(value, buf);
    return toRustBuffer(buf);
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
    final buf = Uint8List(FfiConverterOptionalMoqFrame.allocationSize(value));
    FfiConverterOptionalMoqFrame.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalMoqGrant {
  static MoqGrant? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqGrant.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqGrant?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqGrant.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqGrant?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqGrant? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqGrant.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqGrant? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(FfiConverterOptionalMoqGrant.allocationSize(value));
    FfiConverterOptionalMoqGrant.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqGrant? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqGrant.write(
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
    final buf = Uint8List(
      FfiConverterOptionalMoqGroupConsumer.allocationSize(value),
    );
    FfiConverterOptionalMoqGroupConsumer.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalMoqJsonValue {
  static MoqJsonValue? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqJsonValue.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqJsonValue?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqJsonValue.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqJsonValue?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqJsonValue? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqJsonValue.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqJsonValue? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqJsonValue.allocationSize(value),
    );
    FfiConverterOptionalMoqJsonValue.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqJsonValue? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqJsonValue.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqJsonWindowTimed {
  static MoqJsonWindowTimed? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqJsonWindowTimed.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqJsonWindowTimed?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqJsonWindowTimed.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqJsonWindowTimed?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqJsonWindowTimed? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqJsonWindowTimed.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqJsonWindowTimed? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqJsonWindowTimed.allocationSize(value),
    );
    FfiConverterOptionalMoqJsonWindowTimed.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqJsonWindowTimed? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqJsonWindowTimed.write(
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
    final buf = Uint8List(
      FfiConverterOptionalMoqMediaFrame.allocationSize(value),
    );
    FfiConverterOptionalMoqMediaFrame.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalMoqMediaRead {
  static MoqMediaRead? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqMediaRead.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqMediaRead?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqMediaRead.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqMediaRead?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqMediaRead? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqMediaRead.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqMediaRead? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqMediaRead.allocationSize(value),
    );
    FfiConverterOptionalMoqMediaRead.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqMediaRead? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqMediaRead.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqMediaTimebase {
  static MoqMediaTimebase? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqMediaTimebase.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqMediaTimebase?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqMediaTimebase.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqMediaTimebase?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqMediaTimebase? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqMediaTimebase.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqMediaTimebase? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqMediaTimebase.allocationSize(value),
    );
    FfiConverterOptionalMoqMediaTimebase.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqMediaTimebase? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqMediaTimebase.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqOrder {
  static MoqOrder? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqOrder.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqOrder?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqOrder.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqOrder?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqOrder? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqOrder.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqOrder? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(FfiConverterOptionalMoqOrder.allocationSize(value));
    FfiConverterOptionalMoqOrder.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqOrder? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqOrder.write(
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
    final buf = Uint8List(
      FfiConverterOptionalMoqOriginProducer.allocationSize(value),
    );
    FfiConverterOptionalMoqOriginProducer.write(value, buf);
    return toRustBuffer(buf);
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
    final buf = Uint8List(FfiConverterOptionalMoqRequest.allocationSize(value));
    FfiConverterOptionalMoqRequest.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalMoqReservation {
  static MoqReservation? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqReservation.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqReservation?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqReservation.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqReservation?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqReservation? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqReservation.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqReservation? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqReservation.allocationSize(value),
    );
    FfiConverterOptionalMoqReservation.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqReservation? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqReservation.write(
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
    final buf = Uint8List(
      FfiConverterOptionalMoqSubscription.allocationSize(value),
    );
    FfiConverterOptionalMoqSubscription.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalMoqTokens {
  static MoqTokens? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqTokens.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqTokens?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqTokens.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqTokens?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqTokens? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqTokens.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqTokens? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(FfiConverterOptionalMoqTokens.allocationSize(value));
    FfiConverterOptionalMoqTokens.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqTokens? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqTokens.write(
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
    final buf = Uint8List(
      FfiConverterOptionalMoqTrackInfo.allocationSize(value),
    );
    FfiConverterOptionalMoqTrackInfo.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalMoqVideoGop {
  static MoqVideoGop? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqVideoGop.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoGop?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqVideoGop.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqVideoGop?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqVideoGop? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqVideoGop.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqVideoGop? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqVideoGop.allocationSize(value),
    );
    FfiConverterOptionalMoqVideoGop.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoGop? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqVideoGop.write(
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
    final buf = Uint8List(
      FfiConverterOptionalMoqVideoHint.allocationSize(value),
    );
    FfiConverterOptionalMoqVideoHint.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalMoqVideoProducer {
  static MoqVideoProducer? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqVideoProducer.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoProducer?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqVideoProducer.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqVideoProducer?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqVideoProducer? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqVideoProducer.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqVideoProducer? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqVideoProducer.allocationSize(value),
    );
    FfiConverterOptionalMoqVideoProducer.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoProducer? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqVideoProducer.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqVideoRead {
  static MoqVideoRead? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqVideoRead.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoRead?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqVideoRead.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqVideoRead?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqVideoRead? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqVideoRead.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqVideoRead? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqVideoRead.allocationSize(value),
    );
    FfiConverterOptionalMoqVideoRead.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoRead? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqVideoRead.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalMoqVideoSurface {
  static MoqVideoSurface? lift(RustBuffer buf) {
    return FfiConverterOptionalMoqVideoSurface.read(buf.asUint8List()).value;
  }

  static LiftRetVal<MoqVideoSurface?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterMoqVideoSurface.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<MoqVideoSurface?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([MoqVideoSurface? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterMoqVideoSurface.allocationSize(value) + 1;
  }

  static RustBuffer lower(MoqVideoSurface? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalMoqVideoSurface.allocationSize(value),
    );
    FfiConverterOptionalMoqVideoSurface.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(MoqVideoSurface? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterMoqVideoSurface.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalSequenceMoqRange {
  static List<MoqRange>? lift(RustBuffer buf) {
    return FfiConverterOptionalSequenceMoqRange.read(buf.asUint8List()).value;
  }

  static LiftRetVal<List<MoqRange>?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterSequenceMoqRange.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<List<MoqRange>?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([List<MoqRange>? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterSequenceMoqRange.allocationSize(value) + 1;
  }

  static RustBuffer lower(List<MoqRange>? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalSequenceMoqRange.allocationSize(value),
    );
    FfiConverterOptionalSequenceMoqRange.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(List<MoqRange>? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterSequenceMoqRange.write(
          value,
          Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
        ) +
        1;
  }
}

class FfiConverterOptionalSequenceString {
  static List<String>? lift(RustBuffer buf) {
    return FfiConverterOptionalSequenceString.read(buf.asUint8List()).value;
  }

  static LiftRetVal<List<String>?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterSequenceString.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<List<String>?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([List<String>? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterSequenceString.allocationSize(value) + 1;
  }

  static RustBuffer lower(List<String>? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(
      FfiConverterOptionalSequenceString.allocationSize(value),
    );
    FfiConverterOptionalSequenceString.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(List<String>? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterSequenceString.write(
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
    final buf = Uint8List(FfiConverterOptionalString.allocationSize(value));
    FfiConverterOptionalString.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterOptionalUInt32 {
  static int? lift(RustBuffer buf) {
    return FfiConverterOptionalUInt32.read(buf.asUint8List()).value;
  }

  static LiftRetVal<int?> read(Uint8List buf) {
    if (ByteData.view(buf.buffer, buf.offsetInBytes).getInt8(0) == 0) {
      return LiftRetVal(null, 1);
    }
    final result = FfiConverterUInt32.read(
      Uint8List.view(buf.buffer, buf.offsetInBytes + 1),
    );
    return LiftRetVal<int?>(result.value, result.bytesRead + 1);
  }

  static int allocationSize([int? value]) {
    if (value == null) {
      return 1;
    }
    return FfiConverterUInt32.allocationSize(value) + 1;
  }

  static RustBuffer lower(int? value) {
    if (value == null) {
      return toRustBuffer(Uint8List.fromList([0]));
    }
    final buf = Uint8List(FfiConverterOptionalUInt32.allocationSize(value));
    FfiConverterOptionalUInt32.write(value, buf);
    return toRustBuffer(buf);
  }

  static int write(int? value, Uint8List buf) {
    if (value == null) {
      buf[0] = 0;
      return 1;
    }
    buf[0] = 1;
    return FfiConverterUInt32.write(
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
    final buf = Uint8List(FfiConverterOptionalUInt64.allocationSize(value));
    FfiConverterOptionalUInt64.write(value, buf);
    return toRustBuffer(buf);
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
    final buf = Uint8List(FfiConverterOptionalUint8List.allocationSize(value));
    FfiConverterOptionalUint8List.write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterSequenceDouble32 {
  static List<double> lift(RustBuffer buf) {
    return FfiConverterSequenceDouble32.read(buf.asUint8List()).value;
  }

  static LiftRetVal<List<double>> read(Uint8List buf) {
    List<double> res = [];
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < length; i++) {
      final ret = FfiConverterDouble32.read(Uint8List.view(buf.buffer, offset));
      offset += ret.bytesRead;
      res.add(ret.value);
    }
    return LiftRetVal(res, offset - buf.offsetInBytes);
  }

  static int write(List<double> value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < value.length; i++) {
      offset += FfiConverterDouble32.write(
        value[i],
        Uint8List.view(buf.buffer, offset),
      );
    }
    return offset - buf.offsetInBytes;
  }

  static int allocationSize(List<double> value) {
    return value
            .map((l) => FfiConverterDouble32.allocationSize(l))
            .fold(0, (a, b) => a + b) +
        4;
  }

  static RustBuffer lower(List<double> value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }
}

class FfiConverterSequenceMoqAudioPacket {
  static List<MoqAudioPacket> lift(RustBuffer buf) {
    return FfiConverterSequenceMoqAudioPacket.read(buf.asUint8List()).value;
  }

  static LiftRetVal<List<MoqAudioPacket>> read(Uint8List buf) {
    List<MoqAudioPacket> res = [];
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < length; i++) {
      final ret = FfiConverterMoqAudioPacket.read(
        Uint8List.view(buf.buffer, offset),
      );
      offset += ret.bytesRead;
      res.add(ret.value);
    }
    return LiftRetVal(res, offset - buf.offsetInBytes);
  }

  static int write(List<MoqAudioPacket> value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < value.length; i++) {
      offset += FfiConverterMoqAudioPacket.write(
        value[i],
        Uint8List.view(buf.buffer, offset),
      );
    }
    return offset - buf.offsetInBytes;
  }

  static int allocationSize(List<MoqAudioPacket> value) {
    return value
            .map((l) => FfiConverterMoqAudioPacket.allocationSize(l))
            .fold(0, (a, b) => a + b) +
        4;
  }

  static RustBuffer lower(List<MoqAudioPacket> value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }
}

class FfiConverterSequenceMoqRange {
  static List<MoqRange> lift(RustBuffer buf) {
    return FfiConverterSequenceMoqRange.read(buf.asUint8List()).value;
  }

  static LiftRetVal<List<MoqRange>> read(Uint8List buf) {
    List<MoqRange> res = [];
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < length; i++) {
      final ret = FfiConverterMoqRange.read(Uint8List.view(buf.buffer, offset));
      offset += ret.bytesRead;
      res.add(ret.value);
    }
    return LiftRetVal(res, offset - buf.offsetInBytes);
  }

  static int write(List<MoqRange> value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < value.length; i++) {
      offset += FfiConverterMoqRange.write(
        value[i],
        Uint8List.view(buf.buffer, offset),
      );
    }
    return offset - buf.offsetInBytes;
  }

  static int allocationSize(List<MoqRange> value) {
    return value
            .map((l) => FfiConverterMoqRange.allocationSize(l))
            .fold(0, (a, b) => a + b) +
        4;
  }

  static RustBuffer lower(List<MoqRange> value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
  }
}

class FfiConverterSequenceMoqVideoPacket {
  static List<MoqVideoPacket> lift(RustBuffer buf) {
    return FfiConverterSequenceMoqVideoPacket.read(buf.asUint8List()).value;
  }

  static LiftRetVal<List<MoqVideoPacket>> read(Uint8List buf) {
    List<MoqVideoPacket> res = [];
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < length; i++) {
      final ret = FfiConverterMoqVideoPacket.read(
        Uint8List.view(buf.buffer, offset),
      );
      offset += ret.bytesRead;
      res.add(ret.value);
    }
    return LiftRetVal(res, offset - buf.offsetInBytes);
  }

  static int write(List<MoqVideoPacket> value, Uint8List buf) {
    buf.buffer.asByteData(buf.offsetInBytes).setInt32(0, value.length);
    int offset = buf.offsetInBytes + 4;
    for (var i = 0; i < value.length; i++) {
      offset += FfiConverterMoqVideoPacket.write(
        value[i],
        Uint8List.view(buf.buffer, offset),
      );
    }
    return offset - buf.offsetInBytes;
  }

  static int allocationSize(List<MoqVideoPacket> value) {
    return value
            .map((l) => FfiConverterMoqVideoPacket.allocationSize(l))
            .fold(0, (a, b) => a + b) +
        4;
  }

  static RustBuffer lower(List<MoqVideoPacket> value) {
    final buf = Uint8List(allocationSize(value));
    write(value, buf);
    return toRustBuffer(buf);
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

class FfiConverterUint8List {
  static Uint8List lift(RustBuffer value) {
    return FfiConverterUint8List.read(value.asUint8List()).value;
  }

  static LiftRetVal<Uint8List> read(Uint8List buf) {
    final length = buf.buffer.asByteData(buf.offsetInBytes).getInt32(0);
    final bytes = buf.sublist(4, 4 + length);
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

String moqMintEpoch() {
  return rustCallWithLifter(
    (status) => uniffi_moq_ffi_fn_func_moq_mint_epoch(status),
    FfiConverterString.lift,
    null,
  );
}

MoqClientConfig moqClientDefaults() {
  return rustCallWithLifter(
    (status) => uniffi_moq_ffi_fn_func_moq_client_defaults(status),
    FfiConverterMoqClientConfig.lift,
    null,
  );
}

bool moqQlogSupported() {
  return rustCallWithLifter(
    (status) => uniffi_moq_ffi_fn_func_moq_qlog_supported(status),
    FfiConverterBool.lift,
    null,
  );
}

List<String> moqVersions() {
  return rustCallWithLifter(
    (status) => uniffi_moq_ffi_fn_func_moq_versions(status),
    FfiConverterSequenceString.lift,
    null,
  );
}

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqaudiocodec(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqaudiocodec(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqaudiocodec_aac(
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqaudiocodec_opus(
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqaudioconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqaudioconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, RustBuffer, RustBuffer)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqaudioconsumer_subscribe(
  Pointer<Void> broadcast,
  RustBuffer id,
  RustBuffer catalog_audio,
  RustBuffer output,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqaudioconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqaudioconsumer_delay(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqaudioconsumer_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqaudioencoder(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqaudioencoder(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqaudioencoder_new(
  RustBuffer output,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  RustBuffer Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external RustBuffer uniffi_moq_ffi_fn_method_moqaudioencoder_encode(
  Pointer<Void> ptr,
  RustBuffer pcm,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  RustBuffer Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external RustBuffer uniffi_moq_ffi_fn_method_moqaudioencoder_finish(
  Pointer<Void> ptr,
  RustBuffer pcm,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqaudioencoder_frame_size(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqaudioencoder_set_bitrate(
  Pointer<Void> ptr,
  int bitrate,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqaudioencoder_track_init(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqaudioproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqaudioproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqaudioproducer_new(
  Pointer<Void> catalog,
  RustBuffer options,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqaudioproducer_demand(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqaudioproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqaudioproducer_reanchor(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqaudioproducer_reservation(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqaudioproducer_write(
  Pointer<Void> ptr,
  RustBuffer frame,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqgrantconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqgrantconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqgrantconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqgrantconsumer_changed(
  Pointer<Void> ptr,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqgrantconsumer_grant(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqallocator(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqallocator(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(
    Pointer<Void>,
    Pointer<Void>,
    Uint64,
    Pointer<RustCallStatus>,
  )
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqallocator_reserve(
  Pointer<Void> ptr,
  Pointer<Void> demand,
  int max_bps,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqbandwidthconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqbandwidthconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqbandwidthconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqbandwidthconsumer_rate(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbandwidthconsumer_rate_changed(Pointer<Void> ptr);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqreservation(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqreservation(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqreservation_rate(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqreservation_update(
  Pointer<Void> ptr,
  int max_bps,
  Pointer<RustCallStatus> uniffiStatus,
);

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

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqbroadcastconsumer_resolve(
  Pointer<Void> ptr,
  RustBuffer rendition,
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
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqtrackdemand(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqtrackdemand(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqtrackdemand_name(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqtrackdemand_wanted(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackdemand_wanted_changed(
  Pointer<Void> ptr,
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

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_constructor_moqjsonsnapshotconsumer_new(
  Pointer<Void> track,
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
external Pointer<Void> uniffi_moq_ffi_fn_method_moqjsonsnapshotconsumer_latest(
  Pointer<Void> ptr,
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

@Native<
  Pointer<Void> Function(
    Pointer<Void>,
    Pointer<Void>,
    RustBuffer,
    Pointer<RustCallStatus>,
  )
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_constructor_moqjsonsnapshotproducer_new(
  Pointer<Void> broadcast,
  Pointer<Void> track,
  RustBuffer config,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqjsonsnapshotproducer_demand(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonsnapshotproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(Pointer<Void>, RustBuffer, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external void uniffi_moq_ffi_fn_method_moqjsonsnapshotproducer_update(
  Pointer<Void> ptr,
  RustBuffer value,
  RustBuffer timestamp_us,
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

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqjsonstreamconsumer_new(
  Pointer<Void> track,
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

@Native<
  Pointer<Void> Function(Pointer<Void>, Pointer<Void>, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqjsonstreamproducer_new(
  Pointer<Void> broadcast,
  Pointer<Void> track,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(Pointer<Void>, RustBuffer, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external void uniffi_moq_ffi_fn_method_moqjsonstreamproducer_append(
  Pointer<Void> ptr,
  RustBuffer value,
  RustBuffer timestamp_us,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqjsonstreamproducer_demand(
  Pointer<Void> ptr,
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
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqjsonwindowconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqjsonwindowconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqjsonwindowconsumer_new(
  Pointer<Void> track,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonwindowconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqjsonwindowconsumer_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqjsonwindowproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqjsonwindowproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqjsonwindowproducer_new(
  Pointer<Void> track,
  RustBuffer config,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonwindowproducer_cut(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqjsonwindowproducer_demand(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqjsonwindowproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(Pointer<Void>, Uint64, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external void uniffi_moq_ffi_fn_method_moqjsonwindowproducer_pop(
  Pointer<Void> ptr,
  int count,
  RustBuffer timestamp_us,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(Pointer<Void>, RustBuffer, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external void uniffi_moq_ffi_fn_method_moqjsonwindowproducer_push(
  Pointer<Void> ptr,
  RustBuffer value,
  RustBuffer timestamp_us,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediacatalogconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediacatalogconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_constructor_moqmediacatalogconsumer_subscribe(
  Pointer<Void> broadcast,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediacatalogconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqmediacatalogconsumer_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediacatalogproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediacatalogproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_constructor_moqmediacatalogproducer_new(
  Pointer<Void> broadcast,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediacatalogproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqmediacatalogproducer_now(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediacatalogproducer_remove_section(
  Pointer<Void> ptr,
  RustBuffer name,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Void Function(Pointer<Void>, RustBuffer, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external void uniffi_moq_ffi_fn_method_moqmediacatalogproducer_set_section(
  Pointer<Void> ptr,
  RustBuffer name,
  RustBuffer json,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void
uniffi_moq_ffi_fn_method_moqmediacatalogproducer_set_video_properties(
  Pointer<Void> ptr,
  RustBuffer properties,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediacontainerproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediacontainerproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_constructor_moqmediacontainerproducer_new(
  Pointer<Void> broadcast,
  RustBuffer init,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediacontainerproducer_cut(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediacontainerproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediacontainerproducer_seek(
  Pointer<Void> ptr,
  int sequence,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediacontainerproducer_write(
  Pointer<Void> ptr,
  RustBuffer payload,
  Pointer<RustCallStatus> uniffiStatus,
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

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_constructor_moqmediagroupconsumer_fetch(
  Pointer<Void> broadcast,
  RustBuffer config,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqmediagroupconsumer_new(
  Pointer<Void> group,
  RustBuffer container,
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
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediatimebase(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediatimebase(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqmediatimebase_new(
  Pointer<Void> broadcast,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediatrackconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediatrackconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_constructor_moqmediatrackconsumer_subscribe(
  Pointer<Void> broadcast,
  RustBuffer config,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediatrackconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqmediatrackconsumer_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediatrackproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediatrackproducer(
  Pointer<Void> handle,
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
uniffi_moq_ffi_fn_constructor_moqmediatrackproducer_audio(
  Pointer<Void> broadcast,
  RustBuffer target,
  RustBuffer init,
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
uniffi_moq_ffi_fn_constructor_moqmediatrackproducer_video(
  Pointer<Void> broadcast,
  RustBuffer target,
  RustBuffer init,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediatrackproducer_cut(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqmediatrackproducer_demand(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediatrackproducer_discontinuity(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediatrackproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediatrackproducer_seek(
  Pointer<Void> ptr,
  int sequence,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediatrackproducer_write_frame(
  Pointer<Void> ptr,
  RustBuffer frame,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqmediatrackstreamproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqmediatrackstreamproducer(
  Pointer<Void> handle,
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
uniffi_moq_ffi_fn_constructor_moqmediatrackstreamproducer_video(
  Pointer<Void> broadcast,
  RustBuffer target,
  RustBuffer init,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqmediatrackstreamproducer_demand(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediatrackstreamproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqmediatrackstreamproducer_write(
  Pointer<Void> ptr,
  RustBuffer payload,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqannounceconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqannounceconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqannounceconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqannounceconsumer_next(
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
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqbroadcastprepared(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqbroadcastprepared(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqbroadcastprepared_broadcast(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqbroadcastprepared_publish(
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

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqbroadcastrequest_path(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqbroadcastrequest_prepare(
  Pointer<Void> ptr,
  RustBuffer epoch,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint16, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqbroadcastrequest_reject(
  Pointer<Void> ptr,
  int error_code,
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
  RustBuffer config,
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
  RustBuffer config,
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

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqoriginproducer_requests(
  Pointer<Void> ptr,
  RustBuffer prefix,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqoriginrequests(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqoriginrequests(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqoriginrequests_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqoriginrequests_requested_broadcast(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqpublication(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqpublication(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqpublication_announce(
  Pointer<Void> ptr,
  RustBuffer route,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqpublication_broadcast(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqpublication_unannounce(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

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

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqbroadcastproducer_close(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqbroadcastproducer_consume(
  Pointer<Void> ptr,
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

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqbroadcastproducer_requests(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqbroadcastrequests(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqbroadcastrequests(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqbroadcastrequests_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void>
uniffi_moq_ffi_fn_method_moqbroadcastrequests_requested_track(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqfill(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqfill(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqfill_accept(
  Pointer<Void> ptr,
  int sequence,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqfill_drop_groups(
  Pointer<Void> ptr,
  int start,
  int end,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqfill_end_group(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Int8 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqfill_is_used(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Int8 Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqfill_is_wanted(
  Pointer<Void> ptr,
  int sequence,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqfill_max_delay(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqfill_next(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqfill_order(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Int8 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqfill_priority(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqfill_start_frame(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqfill_start_group(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqfill_unused(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Uint64)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqfill_unwanted(
  Pointer<Void> ptr,
  int sequence,
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
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackproducer_demand(
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

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackproducer_requests(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
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
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackrequest_demand(
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
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackrequest_requests(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqtrackrequests(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqtrackrequests(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtrackrequests_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtrackrequests_requested_fill(
  Pointer<Void> ptr,
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

@Native<Pointer<Void> Function(Pointer<Void>, RustBuffer, RustBuffer)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqrequest_accept(
  Pointer<Void> ptr,
  RustBuffer publish,
  RustBuffer consume,
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
external RustBuffer uniffi_moq_ffi_fn_method_moqrequest_peer_implementation(
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

@Native<Pointer<Void> Function(RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqserver_new(
  RustBuffer config,
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

@Native<Pointer<Void> Function(RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqclient_new(
  RustBuffer config,
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

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqsession_allocator(
  Pointer<Void> ptr,
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
external Pointer<Void> uniffi_moq_ffi_fn_method_moqsession_consume(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqsession_grants(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqsession_publish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqsession_recv_bandwidth(
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
external RustBuffer uniffi_moq_ffi_fn_method_moqsession_state(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Uint64)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqsession_state_changed(
  Pointer<Void> ptr,
  int after,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqsession_stats(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqsession_tokens(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqtokens(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqtokens(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqtokens_answered(
  Pointer<Void> ptr,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqtokens_set(
  Pointer<Void> ptr,
  RustBuffer tokens,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqvideoconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqvideoconsumer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, RustBuffer, RustBuffer)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqvideoconsumer_subscribe(
  Pointer<Void> broadcast,
  RustBuffer id,
  RustBuffer catalog_video,
  RustBuffer output,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqvideoconsumer_cancel(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>)>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqvideoconsumer_next(
  Pointer<Void> ptr,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqvideodecodedframe(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqvideodecodedframe(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint32 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqvideodecodedframe_height(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  RustBuffer Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external RustBuffer uniffi_moq_ffi_fn_method_moqvideodecodedframe_pixels(
  Pointer<Void> ptr,
  RustBuffer format,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqvideodecodedframe_surface(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint64 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqvideodecodedframe_timestamp_us(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Uint32 Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external int uniffi_moq_ffi_fn_method_moqvideodecodedframe_width(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqvideoencoder(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqvideoencoder(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(RustBuffer, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqvideoencoder_new(
  RustBuffer input,
  RustBuffer output,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqvideoencoder_cut(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  RustBuffer Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external RustBuffer uniffi_moq_ffi_fn_method_moqvideoencoder_encode(
  Pointer<Void> ptr,
  RustBuffer frame,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqvideoencoder_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqvideoencoder_flush(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqvideoencoder_set_bitrate(
  Pointer<Void> ptr,
  int bitrate,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_clone_moqvideoproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_free_moqvideoproducer(
  Pointer<Void> handle,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<
  Pointer<Void> Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)
>(assetId: _uniffiAssetId)
external Pointer<Void> uniffi_moq_ffi_fn_constructor_moqvideoproducer_new(
  Pointer<Void> catalog,
  RustBuffer options,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqvideoproducer_cut(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Pointer<Void> Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external Pointer<Void> uniffi_moq_ffi_fn_method_moqvideoproducer_demand(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqvideoproducer_finish(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<Void>, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqvideoproducer_reservation(
  Pointer<Void> ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, Uint64, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqvideoproducer_set_bitrate(
  Pointer<Void> ptr,
  int bitrate,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(Pointer<Void>, RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_method_moqvideoproducer_write(
  Pointer<Void> ptr,
  RustBuffer frame,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external RustBuffer uniffi_moq_ffi_fn_method_moqerror_uniffi_trait_display(
  RustBuffer ptr,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Void Function(RustBuffer, Pointer<RustCallStatus>)>(
  assetId: _uniffiAssetId,
)
external void uniffi_moq_ffi_fn_func_moq_log_level(
  RustBuffer level,
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<RustCallStatus>)>(assetId: _uniffiAssetId)
external RustBuffer uniffi_moq_ffi_fn_func_moq_mint_epoch(
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<RustCallStatus>)>(assetId: _uniffiAssetId)
external RustBuffer uniffi_moq_ffi_fn_func_moq_client_defaults(
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<Int8 Function(Pointer<RustCallStatus>)>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_fn_func_moq_qlog_supported(
  Pointer<RustCallStatus> uniffiStatus,
);

@Native<RustBuffer Function(Pointer<RustCallStatus>)>(assetId: _uniffiAssetId)
external RustBuffer uniffi_moq_ffi_fn_func_moq_versions(
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
external int uniffi_moq_ffi_checksum_func_moq_mint_epoch();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_func_moq_client_defaults();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_func_moq_qlog_supported();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_func_moq_versions();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioconsumer_delay();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioencoder_encode();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioencoder_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioencoder_frame_size();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioencoder_set_bitrate();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioencoder_track_init();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioproducer_demand();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioproducer_reanchor();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioproducer_reservation();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqaudioproducer_write();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgrantconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgrantconsumer_changed();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgrantconsumer_grant();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqallocator_reserve();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbandwidthconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbandwidthconsumer_rate();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbandwidthconsumer_rate_changed();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqreservation_rate();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqreservation_update();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_fetch_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_resolve();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_track();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupconsumer_read_frame();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqgroupconsumer_sequence();

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
external int uniffi_moq_ffi_checksum_method_moqtrackdemand_name();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackdemand_wanted();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackdemand_wanted_changed();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_latest();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonsnapshotproducer_demand();

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
external int uniffi_moq_ffi_checksum_method_moqjsonstreamproducer_demand();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonstreamproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonwindowconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonwindowconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_cut();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_demand();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_pop();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_push();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediacatalogconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediacatalogconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_now();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_remove_section();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_set_section();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_set_video_properties();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediacontainerproducer_cut();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediacontainerproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediacontainerproducer_seek();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediacontainerproducer_write();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_sequence();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediatrackconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediatrackconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediatrackproducer_cut();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediatrackproducer_demand();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqmediatrackproducer_discontinuity();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediatrackproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediatrackproducer_seek();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediatrackproducer_write_frame();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqmediatrackstreamproducer_demand();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqmediatrackstreamproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqmediatrackstreamproducer_write();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannounceconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannounceconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_available();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastprepared_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastprepared_publish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastrequest_path();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastrequest_prepare();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastrequest_reject();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqoriginconsumer_request_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqoriginproducer_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqoriginproducer_create_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqoriginproducer_requests();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqoriginrequests_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqoriginrequests_requested_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqpublication_announce();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqpublication_broadcast();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqpublication_unannounce();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastproducer_close();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastproducer_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_track();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastproducer_requests();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqbroadcastrequests_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_method_moqbroadcastrequests_requested_track();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_accept();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_drop_groups();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_end_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_is_used();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_is_wanted();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_max_delay();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_order();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_priority();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_start_frame();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_start_group();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_unused();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqfill_unwanted();

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
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_demand();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_finish_at();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_requests();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackproducer_write_frame();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequest_abort();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequest_accept();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequest_demand();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequest_name();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequest_requests();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequests_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtrackrequests_requested_fill();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_accept();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_path();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_peer_implementation();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_query();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqrequest_reject();

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
external int uniffi_moq_ffi_checksum_method_moqclient_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqclient_connect();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_allocator();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_closed();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_consume();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_grants();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_publish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_recv_bandwidth();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_shutdown();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_state();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_state_changed();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_stats();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqsession_tokens();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtokens_answered();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqtokens_set();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoconsumer_cancel();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoconsumer_next();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideodecodedframe_height();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideodecodedframe_pixels();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideodecodedframe_surface();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideodecodedframe_timestamp_us();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideodecodedframe_width();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoencoder_cut();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoencoder_encode();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoencoder_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoencoder_flush();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoencoder_set_bitrate();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoproducer_cut();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoproducer_demand();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoproducer_finish();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoproducer_reservation();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoproducer_set_bitrate();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_method_moqvideoproducer_write();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqaudiocodec_aac();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqaudiocodec_opus();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqaudioconsumer_subscribe();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqaudioencoder_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqaudioproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqjsonsnapshotconsumer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqjsonsnapshotproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqjsonstreamconsumer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqjsonstreamproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqjsonwindowconsumer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqjsonwindowproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_constructor_moqmediacatalogconsumer_subscribe();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqmediacatalogproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_constructor_moqmediacontainerproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqmediagroupconsumer_fetch();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqmediagroupconsumer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqmediatimebase_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_constructor_moqmediatrackconsumer_subscribe();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqmediatrackproducer_audio();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqmediatrackproducer_video();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int
uniffi_moq_ffi_checksum_constructor_moqmediatrackstreamproducer_video();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqoriginproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqbroadcastproducer_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqserver_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqclient_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqvideoconsumer_subscribe();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqvideoencoder_new();

@Native<Uint16 Function()>(assetId: _uniffiAssetId)
external int uniffi_moq_ffi_checksum_constructor_moqvideoproducer_new();

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
  if (uniffi_moq_ffi_checksum_func_moq_mint_epoch() != 34550) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_func_moq_client_defaults() != 39302) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_func_moq_qlog_supported() != 58291) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_func_moq_versions() != 28954) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioconsumer_cancel() != 62285) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioconsumer_delay() != 48052) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioconsumer_next() != 7650) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioencoder_encode() != 56589) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioencoder_finish() != 24341) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioencoder_frame_size() != 37264) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioencoder_set_bitrate() != 10667) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioencoder_track_init() != 56663) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioproducer_demand() != 42822) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioproducer_finish() != 14521) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioproducer_reanchor() != 34412) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioproducer_reservation() != 50185) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqaudioproducer_write() != 48796) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgrantconsumer_cancel() != 22691) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgrantconsumer_changed() != 37570) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgrantconsumer_grant() != 36088) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqallocator_reserve() != 34785) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbandwidthconsumer_cancel() != 58304) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbandwidthconsumer_rate() != 420) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbandwidthconsumer_rate_changed() !=
      38021) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqreservation_rate() != 65114) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqreservation_update() != 9626) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_fetch_group() !=
      18633) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_resolve() != 56822) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastconsumer_subscribe_track() !=
      2348) {
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
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_cancel() != 65022) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_info() != 32757) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_next_group() != 5449) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_read_frame() != 42799) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_recv_datagram() !=
      17412) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_recv_group() != 12949) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackconsumer_update() != 39083) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackdemand_name() != 14603) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackdemand_wanted() != 13310) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackdemand_wanted_changed() != 29509) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_cancel() !=
      52062) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_latest() !=
      60932) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotconsumer_next() != 37038) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotproducer_demand() !=
      45789) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotproducer_finish() !=
      42593) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonsnapshotproducer_update() !=
      17590) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonstreamconsumer_cancel() != 29308) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonstreamconsumer_next() != 21257) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonstreamproducer_append() != 36078) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonstreamproducer_demand() != 52854) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonstreamproducer_finish() != 51459) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonwindowconsumer_cancel() != 63019) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonwindowconsumer_next() != 52011) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_cut() != 9716) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_demand() != 20175) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_finish() != 49018) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_pop() != 62669) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqjsonwindowproducer_push() != 19295) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacatalogconsumer_cancel() !=
      44590) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacatalogconsumer_next() != 43296) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_finish() !=
      62353) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_now() != 14140) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_remove_section() !=
      36089) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_set_section() !=
      32625) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacatalogproducer_set_video_properties() !=
      6577) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacontainerproducer_cut() != 35477) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacontainerproducer_finish() !=
      40580) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacontainerproducer_seek() != 6771) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediacontainerproducer_write() !=
      17951) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_cancel() != 4852) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_next() != 3173) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediagroupconsumer_sequence() !=
      57049) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackconsumer_cancel() != 17311) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackconsumer_next() != 52053) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackproducer_cut() != 54122) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackproducer_demand() != 51616) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackproducer_discontinuity() !=
      25213) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackproducer_finish() != 22633) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackproducer_seek() != 37244) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackproducer_write_frame() !=
      7616) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackstreamproducer_demand() !=
      23377) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackstreamproducer_finish() !=
      43608) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqmediatrackstreamproducer_write() !=
      44842) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannounceconsumer_cancel() != 10799) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannounceconsumer_next() != 38311) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_available() !=
      47724) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqannouncedbroadcast_cancel() != 63175) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastprepared_broadcast() !=
      42363) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastprepared_publish() != 60136) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastrequest_path() != 6534) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastrequest_prepare() != 14120) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastrequest_reject() != 9727) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced() != 16595) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginconsumer_announced_broadcast() !=
      40619) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginconsumer_request_broadcast() !=
      36974) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginproducer_consume() != 52357) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginproducer_create_broadcast() !=
      43708) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginproducer_requests() != 8499) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginrequests_cancel() != 18954) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqoriginrequests_requested_broadcast() !=
      31253) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqpublication_announce() != 60624) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqpublication_broadcast() != 38236) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqpublication_unannounce() != 21154) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_close() != 26090) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_consume() != 27634) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_publish_track() !=
      44452) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastproducer_requests() != 27464) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastrequests_cancel() != 44028) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqbroadcastrequests_requested_track() !=
      49236) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_accept() != 63990) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_drop_groups() != 7289) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_end_group() != 42504) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_is_used() != 6429) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_is_wanted() != 47156) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_max_delay() != 1258) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_next() != 7725) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_order() != 32232) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_priority() != 23036) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_start_frame() != 29713) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_start_group() != 8622) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_unused() != 3173) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqfill_unwanted() != 13637) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_abort() != 59787) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_consume() != 53274) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_finish() != 61241) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_sequence() != 21067) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqgroupproducer_write_frame() != 42053) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_abort() != 37537) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_append_datagram() !=
      8934) {
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
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_demand() != 7311) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_finish() != 63940) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_finish_at() != 24581) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_requests() != 60252) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackproducer_write_frame() != 32196) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequest_abort() != 62713) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequest_accept() != 26498) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequest_demand() != 3618) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequest_name() != 56715) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequest_requests() != 23724) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequests_cancel() != 43316) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtrackrequests_requested_fill() !=
      55377) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_accept() != 55136) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_cancel() != 25859) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_path() != 48052) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_peer_implementation() !=
      48833) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_query() != 23842) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_reject() != 2829) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_transport() != 57171) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqrequest_url() != 34138) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_accept() != 44310) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_cancel() != 56970) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_cert_fingerprints() != 57398) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqserver_listen() != 9040) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_cancel() != 29949) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqclient_connect() != 5016) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_allocator() != 12118) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_cancel() != 39476) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_closed() != 9326) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_consume() != 57909) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_grants() != 48347) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_publish() != 15240) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_recv_bandwidth() != 46568) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_shutdown() != 32451) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_state() != 32292) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_state_changed() != 59452) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_stats() != 36396) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqsession_tokens() != 2396) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtokens_answered() != 11571) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqtokens_set() != 20110) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoconsumer_cancel() != 21122) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoconsumer_next() != 21151) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideodecodedframe_height() != 7330) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideodecodedframe_pixels() != 60854) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideodecodedframe_surface() != 3872) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideodecodedframe_timestamp_us() !=
      18681) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideodecodedframe_width() != 30916) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoencoder_cut() != 28527) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoencoder_encode() != 513) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoencoder_finish() != 5918) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoencoder_flush() != 4585) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoencoder_set_bitrate() != 34315) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoproducer_cut() != 18974) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoproducer_demand() != 283) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoproducer_finish() != 40208) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoproducer_reservation() != 1052) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoproducer_set_bitrate() != 19081) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_method_moqvideoproducer_write() != 17427) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqaudiocodec_aac() != 62161) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqaudiocodec_opus() != 64803) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqaudioconsumer_subscribe() !=
      10630) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqaudioencoder_new() != 52616) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqaudioproducer_new() != 51486) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqjsonsnapshotconsumer_new() !=
      11929) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqjsonsnapshotproducer_new() !=
      14914) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqjsonstreamconsumer_new() != 3025) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqjsonstreamproducer_new() !=
      20445) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqjsonwindowconsumer_new() !=
      48790) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqjsonwindowproducer_new() !=
      43418) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediacatalogconsumer_subscribe() !=
      2004) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediacatalogproducer_new() !=
      4684) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediacontainerproducer_new() !=
      41369) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediagroupconsumer_fetch() !=
      32566) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediagroupconsumer_new() !=
      58198) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediatimebase_new() != 21606) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediatrackconsumer_subscribe() !=
      55638) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediatrackproducer_audio() !=
      8819) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediatrackproducer_video() !=
      16061) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqmediatrackstreamproducer_video() !=
      39137) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqoriginproducer_new() != 48126) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqbroadcastproducer_new() != 12099) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqserver_new() != 49910) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqclient_new() != 48355) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqvideoconsumer_subscribe() !=
      15507) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqvideoencoder_new() != 22531) {
    throw UniffiInternalError.panicked("UniFFI API checksum mismatch");
  }
  if (uniffi_moq_ffi_checksum_constructor_moqvideoproducer_new() != 51291) {
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
