# Building PlasmaTrack

Every tool below is FOSS.

## Rust workspace (pt-core, client; later the server)

On Windows, use the **gnullvm** host toolchain together with llvm-mingw (the same
toolchain as the plugin). Bundled C code (SQLite) and Rust then share one UCRT-based
toolchain, and no proprietary MSVC Build Tools are needed. `.cargo/config.toml` links
the runtime statically, so the binaries import only Windows system DLLs.

```powershell
winget install Rustlang.Rustup
rustup toolchain install stable-x86_64-pc-windows-gnullvm --profile minimal -c clippy,rustfmt
rustup default stable-x86_64-pc-windows-gnullvm
$env:PATH = "$env:LOCALAPPDATA\PlasmaTrackTools\llvm-mingw-<date>-ucrt-x86_64\bin;$env:PATH"   # llvm-mingw, see below
cargo test --workspace
```

On Linux, install Rust with rustup (https://rustup.rs) and a C compiler (for SQLite), then run `cargo test --workspace`.

End-to-end check of the real plugin DLL against the real client binary (Windows):
`powershell -ExecutionPolicy Bypass -File scripts\e2e-plugin-client.ps1`.

## Plugin: Windows DLL, built on Windows

Tools: **llvm-mingw (UCRT)**, **CMake ≥ 3.20** and **Ninja**. Either:

- `winget install MartinStorsjo.LLVM-MinGW.UCRT Kitware.CMake Ninja-build.Ninja`
  (the CMake MSI asks for admin rights), or
- no admin rights needed: unpack the official release zips into `%LOCALAPPDATA%\PlasmaTrackTools`:
  - `cmake-<ver>-windows-x86_64.zip` from github.com/Kitware/CMake/releases
  - `ninja-win.zip` from github.com/ninja-build/ninja/releases
  - `llvm-mingw-<date>-ucrt-x86_64.zip` from github.com/mstorsjo/llvm-mingw/releases

Then run:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-plugin.ps1
```

This builds `plugin\build-win\plasmatrack.dll` and runs three test suites:
- **unit tests:** SHA-256 vectors, JSON, SPSC ring, settings, journal;
- **fake-game harness:** loads the real DLL, drives the SDK callbacks, and acts as the client over loopback;
- **import/export check:** the DLL may import only Windows system DLLs, and must export `scs_telemetry_init` / `scs_telemetry_shutdown`.

Verified on Windows 11 with llvm-mingw 20260922 (clang 23.1.2), CMake 4.4.3 and Ninja 1.13.2.

## Plugin: Windows DLL, cross-built on Linux (MinGW-w64)

```sh
# Recommended: llvm-mingw release tarball
export LLVM_MINGW_ROOT=/opt/llvm-mingw-<date>-ucrt-ubuntu-<ver>-x86_64
scripts/build-plugin.sh win

# Alternative: distro GCC (Debian/Ubuntu)
sudo apt install g++-mingw-w64-x86-64-posix cmake ninja-build
scripts/build-plugin.sh win
```

- **Toolchain files:** `plugin/cmake/toolchain-llvm-mingw.cmake` and `plugin/cmake/toolchain-debian-mingw.cmake`.
- **Tests:** they run under `wine` if it is installed, and are skipped otherwise.
- **Distro GCC:** the `-posix` compiler variant is used explicitly. ubuntu-24.04's distro mingw
  targets msvcrt, not UCRT, and that is fine for the game.

**Not yet verified:** the Linux cross-build has not been run. The dev machine is Windows 11
with no WSL. The first Linux run should use `scripts/build-plugin.sh win` with wine installed,
so that all three test suites run.

## Plugin: native Linux `.so` (secondary target)

```sh
sudo apt install g++ cmake ninja-build
scripts/build-plugin.sh linux     # plugin/build-linux/plasmatrack.so
```

- Exports are restricted to the two SDK entry points (`plugin/plasmatrack.map`).
- libstdc++ and libgcc are linked statically.
- **Not yet verified** for the same reason as the cross-build.

## Release checklist (plugin)

1. Build with tests passing on Windows and (once available) Linux.
2. Record the SHA-256 of every shipped file in the release manifest (rule 0, PLAN §6a).
3. Ship `THIRD_PARTY_NOTICES.md` and `third_party/notices/` with the binaries.
