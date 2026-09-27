#!/usr/bin/env sh
# Builds the PlasmaTrack plugin on Linux.
#   scripts/build-plugin.sh win     Windows x64 DLL (primary; used under Proton)
#                                   - with LLVM_MINGW_ROOT set: llvm-mingw (recommended)
#                                   - otherwise: Debian/Ubuntu g++-mingw-w64-x86-64-posix
#                                   Tests run under wine if it is installed.
#   scripts/build-plugin.sh linux   native .so (secondary target)
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
target=${1:-win}

case "$target" in
  win)
    build="$root/plugin/build-cross"
    if [ -n "${LLVM_MINGW_ROOT:-}" ]; then
      tc="$root/plugin/cmake/toolchain-llvm-mingw.cmake"
      set -- -DLLVM_MINGW_ROOT="$LLVM_MINGW_ROOT"
    else
      tc="$root/plugin/cmake/toolchain-debian-mingw.cmake"
      set --
    fi
    cmake -S "$root/plugin" -B "$build" -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_TOOLCHAIN_FILE="$tc" "$@"
    ;;
  linux)
    build="$root/plugin/build-linux"
    cmake -S "$root/plugin" -B "$build" -G Ninja -DCMAKE_BUILD_TYPE=Release
    ;;
  *) echo "usage: $0 [win|linux]" >&2; exit 2 ;;
esac

cmake --build "$build"
if [ "$target" = linux ] || command -v wine64 >/dev/null 2>&1 || command -v wine >/dev/null 2>&1; then
  ctest --test-dir "$build" --output-on-failure
else
  echo "wine not found: skipping tests of the Windows DLL"
fi
ls -l "$build"/plasmatrack.*
