import 'package:moq_ffi/moq_ffi.dart';

/// Mint a fresh publisher epoch from the wall clock and secure randomness,
/// ordered newest last. Mint one per publisher run and announce it in
/// `AnnounceRoute.epoch`, so viewers see a restart as a new broadcast instead of a
/// stalled one.
String mintEpoch() => moqMintEpoch();
