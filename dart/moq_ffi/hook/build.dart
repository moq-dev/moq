import 'dart:io';

import 'package:code_assets/code_assets.dart';
import 'package:crypto/crypto.dart';
import 'package:hooks/hooks.dart';
import 'package:http/http.dart' as http;

/// The moq-ffi release whose native assets this package downloads.
///
/// dart/scripts/package.sh rewrites this when staging a release. The sentinel
/// is never used to download: a checkout builds from source instead, and a
/// package built without the injection fails loudly rather than fetching a
/// library that does not match these bindings.
const ffiVersion = '0.0.0-dev';
const assetId = 'uniffi:moq_ffi';

Future<void> main(List<String> args) async {
  await build(args, (input, output) async {
    if (!input.config.buildCodeAssets) return;

    final code = input.config.code;
    final target = _target(code);
    final library = _libraryName(code.targetOS);
    final resolved = await _resolveLibrary(input, target, library);
    final bundled = input.outputDirectory.resolve(library);

    await File.fromUri(resolved.library).copy(bundled.toFilePath());
    output.dependencies.add(resolved.library);
    output.dependencies.addAll(resolved.inputs);
    output.assets.code.add(
      CodeAsset(
        package: input.packageName,
        name: assetId,
        file: bundled,
        // StaticLinking is declared by code_assets but unimplemented in the
        // Dart and Flutter SDK (dart-lang/sdk#49418), so every platform,
        // iOS included, ships a dynamic library.
        linkMode: DynamicLoadingBundled(),
      ),
    );
  });
}

/// A resolved native library plus anything else the hook must re-run for.
///
/// A downloaded library has none: it is pinned by version and checksum. A
/// library built from this checkout lists its Rust sources instead, so an edit
/// there invalidates the cached asset.
typedef _Resolved = ({Uri library, List<Uri> inputs});

Future<_Resolved> _resolveLibrary(
  BuildInput input,
  String target,
  String library,
) async {
  // Hooks run with a filtered environment, so an env var cannot reach here.
  // A consumer pins their own build through pubspec user-defines:
  //
  //   hooks:
  //     user_defines:
  //       moq_ffi:
  //         library: path/to/libmoq_ffi.dylib
  final pinned = input.userDefines.path('library');
  if (pinned != null) {
    final file = File.fromUri(pinned);
    if (!await file.exists()) {
      throw StateError('moq_ffi library user-define does not exist: $pinned');
    }
    return (library: file.uri, inputs: const <Uri>[]);
  }

  final workspace = input.packageRoot.resolve('../../');
  final manifest = workspace.resolve('rs/moq-ffi/Cargo.toml');
  if (await File.fromUri(manifest).exists()) {
    return _build(workspace, target, library);
  }

  return (
    library: await _download(input.outputDirectoryShared, target, library),
    inputs: const <Uri>[],
  );
}

/// Build moq-ffi from this checkout.
///
/// Always shells out to Cargo rather than reusing whatever is already in
/// target/: an existing artifact may predate a Rust edit, or have been built by
/// another recipe with different features. Cargo's own timestamp check makes
/// the no-op case cheap.
Future<_Resolved> _build(Uri workspace, String target, String library) async {
  final cargoArgs = [
    'build',
    '--locked',
    '--release',
    '--package',
    'moq-ffi',
    '--no-default-features',
    '--manifest-path',
    workspace.resolve('Cargo.toml').toFilePath(),
  ];

  // Android needs the NDK's linker and sysroot, which only cargo-ndk sets up.
  // This mirrors rs/moq-ffi/build.sh.
  final args = target.contains('-android')
      ? ['ndk', '--target', target, '--platform', '24', '--', ...cargoArgs]
      : [...cargoArgs, '--target', target];

  final result = await Process.run('cargo', args);
  if (result.exitCode != 0) {
    throw StateError(
      'cargo ${args.join(' ')} failed:\n${result.stdout}\n${result.stderr}',
    );
  }

  final built = workspace.resolve('target/$target/release/$library');
  if (!await File.fromUri(built).exists()) {
    throw StateError('cargo did not produce ${built.toFilePath()}');
  }

  return (library: built, inputs: await _rustInputs(workspace));
}

/// Every Rust source and manifest the build reads, so an edit anywhere in the
/// workspace invalidates the cached asset rather than just an edit to moq-ffi.
Future<List<Uri>> _rustInputs(Uri workspace) async {
  final inputs = <Uri>[];

  // Anything Cargo consults, not just the crate sources: a [profile.release]
  // edit or a toolchain bump changes the artifact without touching rs/.
  for (final name in const [
    'Cargo.lock',
    'Cargo.toml',
    'rust-toolchain.toml',
    '.cargo/config.toml',
  ]) {
    final file = File.fromUri(workspace.resolve(name));
    if (await file.exists()) inputs.add(file.uri);
  }

  final sources = Directory.fromUri(workspace.resolve('rs/'));
  await for (final entry in sources.list(recursive: true, followLinks: false)) {
    if (entry is! File) continue;
    final name = entry.uri.pathSegments.last;
    if (name.endsWith('.rs') || name == 'Cargo.toml') inputs.add(entry.uri);
  }

  return inputs;
}

Future<Uri> _download(Uri cache, String target, String library) async {
  final name = 'moq-ffi-$ffiVersion-$target-$library';
  final cached = cache.resolve(name);
  final cachedFile = File.fromUri(cached);
  final cachedSum = File.fromUri(cache.resolve('$name.sha256'));

  // Re-verify rather than trusting the file's presence. A build killed
  // mid-write leaves a truncated library that would otherwise be copied into
  // the application and only fail later, when the native load fails.
  if (await cachedFile.exists() && await cachedSum.exists()) {
    final want = (await cachedSum.readAsString()).trim();
    if (sha256.convert(await cachedFile.readAsBytes()).toString() == want) {
      return cached;
    }
  }

  final base = Uri.parse(
    'https://github.com/moq-dev/moq/releases/download/'
    'moq-ffi-v$ffiVersion/',
  );
  final asset = base.resolve(name);
  final checksum = base.resolve('$name.sha256');
  final responses = await Future.wait([http.get(asset), http.get(checksum)]);
  if (responses.any((response) => response.statusCode != 200)) {
    throw StateError(
      'failed to download $asset '
      '(library ${responses[0].statusCode}, checksum ${responses[1].statusCode})',
    );
  }

  final expected = responses[1].body.trim().split(RegExp(r'\s+')).first;
  final actual = sha256.convert(responses[0].bodyBytes).toString();
  if (actual != expected) {
    throw StateError(
      'checksum mismatch for $asset: expected $expected, got $actual',
    );
  }

  // Write to a sibling and rename, so a concurrent or interrupted build never
  // observes a partial file at the real path.
  final staging = File.fromUri(cache.resolve('$name.${pid}.part'));
  await staging.create(recursive: true);
  await staging.writeAsBytes(responses[0].bodyBytes, flush: true);
  await staging.rename(cachedFile.path);
  await cachedSum.writeAsString(actual, flush: true);
  return cached;
}

String _target(CodeConfig code) {
  final arch = code.targetArchitecture;
  return switch (code.targetOS) {
    OS.android => switch (arch) {
      Architecture.arm => 'armv7-linux-androideabi',
      Architecture.arm64 => 'aarch64-linux-android',
      Architecture.x64 => 'x86_64-linux-android',
      _ => throw UnsupportedError('unsupported Android architecture: $arch'),
    },
    OS.iOS => switch ((arch, code.iOS.targetSdk)) {
      (Architecture.arm64, IOSSdk.iPhoneOS) => 'aarch64-apple-ios',
      (Architecture.arm64, IOSSdk.iPhoneSimulator) => 'aarch64-apple-ios-sim',
      (Architecture.x64, IOSSdk.iPhoneSimulator) => 'x86_64-apple-ios',
      _ => throw UnsupportedError('unsupported iOS target: $arch'),
    },
    OS.linux => switch (arch) {
      Architecture.arm64 => 'aarch64-unknown-linux-gnu',
      Architecture.x64 => 'x86_64-unknown-linux-gnu',
      _ => throw UnsupportedError('unsupported Linux architecture: $arch'),
    },
    OS.macOS => switch (arch) {
      Architecture.arm64 => 'aarch64-apple-darwin',
      Architecture.x64 => 'x86_64-apple-darwin',
      _ => throw UnsupportedError('unsupported macOS architecture: $arch'),
    },
    OS.windows => switch (arch) {
      Architecture.x64 => 'x86_64-pc-windows-msvc',
      _ => throw UnsupportedError('unsupported Windows architecture: $arch'),
    },
    _ => throw UnsupportedError(
      'unsupported operating system: ${code.targetOS}',
    ),
  };
}

String _libraryName(OS os) => switch (os) {
  OS.iOS || OS.macOS => 'libmoq_ffi.dylib',
  OS.android || OS.linux => 'libmoq_ffi.so',
  OS.windows => 'moq_ffi.dll',
  _ => throw UnsupportedError('unsupported operating system: $os'),
};
