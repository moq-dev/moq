@echo off
setlocal enabledelayedexpansion
REM ---------------------------------------------------------------------------
REM MoQ developer setup for Windows.
REM
REM Installs the toolchain needed to build the Rust workspace and JS packages
REM using winget. Safe to re-run: winget skips or upgrades anything already
REM present. See doc/setup/windows.md for the manual steps and known caveats.
REM ---------------------------------------------------------------------------

echo === MoQ Windows setup ===
echo.
echo Tip: on a fresh machine, run this from an Administrator terminal so winget
echo can install Git and the Visual Studio Build Tools, which need elevation.
echo.

where winget >nul 2>nul
if errorlevel 1 (
	echo ERROR: winget not found.
	echo Install "App Installer" from the Microsoft Store ^(or Windows Update^), then re-run.
	exit /b 1
)

REM No --disable-interactivity: let winget prompt for elevation (UAC) so Git and
REM the Build Tools can install on a fresh, non-Administrator machine.
set "WG=winget install -e --source winget --accept-package-agreements --accept-source-agreements"

echo --- Git ---
%WG% --id Git.Git
echo --- Rust ^(rustup^) ---
%WG% --id Rustlang.Rustup
echo --- Bun ---
%WG% --id Oven-sh.Bun
echo --- Node.js LTS ---
%WG% --id OpenJS.NodeJS.LTS
echo --- just ---
%WG% --id Casey.Just
echo --- CMake ---
%WG% --id Kitware.CMake
echo --- FFmpeg ^(CLI, for the `just dev` demo media^) ---
%WG% --id Gyan.FFmpeg
echo --- GitHub CLI ---
%WG% --id GitHub.cli
echo --- uv ^(Python, for the py/ workspace^) ---
%WG% --id astral-sh.uv

REM MSVC linker + C++ headers, required for any Rust MSVC build. winget skips
REM this when it's already installed and current. The --override adds the C++
REM workload on a fresh install (a bare Build Tools install has no compiler).
echo --- Visual Studio Build Tools ^(C++ workload^) ---
%WG% --id Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

echo.
echo === Updating the Rust toolchain ^(the workspace needs a recent stable^) ===
where rustup >nul 2>nul && ( rustup default stable & rustup update stable ) || echo NOTE: rustup not on PATH yet. Reopen your terminal, then run: rustup update stable

echo.
echo === Installing JS dependencies ^(bun install^) ===
where bun >nul 2>nul && bun install || echo NOTE: bun not on PATH yet. Reopen your terminal, then run: bun install

echo.
echo === Done ===
echo If anything above said "not on PATH yet", CLOSE and REOPEN your terminal so
echo the freshly installed tools are picked up, then re-run this script.
echo.
echo Next steps:
echo   cargo build                                     ^(build the Rust workspace^)
echo   cargo run --bin moq-relay -- demo/relay/localhost.toml   ^(run a local relay^)
echo.
echo See doc/setup/windows.md for running the demo components and the `just` PATH note.

endlocal
