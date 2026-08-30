import 'dart:io';

import 'package:code_assets/code_assets.dart';
import 'package:crypto/crypto.dart';
import 'package:hooks/hooks.dart';
import 'package:http/http.dart' as http;

const ffiVersion = '0.3.14';
const assetId = 'uniffi:moq_ffi';

Future<void> main(List<String> args) async {
  await build(args, (input, output) async {
    if (!input.config.buildCodeAssets) return;

    final code = input.config.code;
    final target = _target(code);
    final library = _libraryName(code.targetOS);
    final source = await _resolveLibrary(input, target, library);
    final bundled = input.outputDirectory.resolve(library);

    await File.fromUri(source).copy(bundled.toFilePath());
    output.dependencies.add(source);
    output.assets.code.add(
      CodeAsset(
        package: input.packageName,
        name: assetId,
        file: bundled,
        linkMode: code.targetOS == OS.iOS
            ? StaticLinking()
            : DynamicLoadingBundled(),
      ),
    );
  });
}

Future<Uri> _resolveLibrary(
  BuildInput input,
  String target,
  String library,
) async {
  final override = Platform.environment['MOQ_DART_FFI_LIB'];
  if (override != null) {
    final file = File(override);
    if (!await file.exists()) {
      throw StateError('MOQ_DART_FFI_LIB does not exist: $override');
    }
    return file.uri;
  }

  final workspace = input.packageRoot.resolve('../../');
  final manifest = workspace.resolve('rs/moq-ffi/Cargo.toml');
  if (await File.fromUri(manifest).exists()) {
    final built = workspace.resolve('target/$target/release/$library');
    if (await File.fromUri(built).exists()) return built;

    final result = await Process.run('cargo', [
      'build',
      '--locked',
      '--release',
      '--package',
      'moq-ffi',
      '--no-default-features',
      '--target',
      target,
      '--manifest-path',
      workspace.resolve('Cargo.toml').toFilePath(),
    ]);
    if (result.exitCode != 0) {
      throw StateError(
        'cargo build failed:\n${result.stdout}\n${result.stderr}',
      );
    }

    if (!await File.fromUri(built).exists()) {
      throw StateError('cargo did not produce ${built.toFilePath()}');
    }
    return built;
  }

  return _download(input.outputDirectoryShared, target, library);
}

Future<Uri> _download(Uri cache, String target, String library) async {
  final name = 'moq-ffi-$ffiVersion-$target-$library';
  final cached = cache.resolve(name);
  final cachedFile = File.fromUri(cached);
  if (await cachedFile.exists()) return cached;

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

  await cachedFile.create(recursive: true);
  await cachedFile.writeAsBytes(responses[0].bodyBytes, flush: true);
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
  OS.iOS => 'libmoq_ffi.a',
  OS.android || OS.linux => 'libmoq_ffi.so',
  OS.macOS => 'libmoq_ffi.dylib',
  OS.windows => 'moq_ffi.dll',
  _ => throw UnsupportedError('unsupported operating system: $os'),
};
