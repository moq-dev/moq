import 'package:moq_ffi/moq_ffi.dart';

/// Mint a fresh publisher epoch from the wall clock and secure randomness,
/// ordered newest last. Mint one per publisher run and announce it in
/// `MoqRoute.epoch`, so viewers see a restart as a new broadcast instead of a
/// stalled one.
String mintEpoch() => moqMintEpoch();

/// The UTC wall-clock time [epoch] encodes, to the millisecond. Throws unless
/// it is a lowercase hyphenated UUIDv7.
DateTime epochTime(String epoch) => DateTime.fromMillisecondsSinceEpoch(
  moqEpochTimeMs(epoch: epoch),
  isUtc: true,
);
