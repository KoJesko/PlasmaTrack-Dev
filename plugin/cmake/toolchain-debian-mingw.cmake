# Cross toolchain for Debian/Ubuntu mingw-w64 GCC (packages g++-mingw-w64-x86-64-posix).
# The -posix compiler variant is named explicitly: the plain name defaults to the
# win32 thread model on Debian/Ubuntu. ubuntu-24.04 ships msvcrt-only targets.
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR x86_64)

set(_triple x86_64-w64-mingw32)
set(CMAKE_C_COMPILER   ${_triple}-gcc-posix)
set(CMAKE_CXX_COMPILER ${_triple}-g++-posix)
set(CMAKE_RC_COMPILER  ${_triple}-windres)
set(PT_OBJDUMP         ${_triple}-objdump CACHE FILEPATH "")

set(CMAKE_FIND_ROOT_PATH /usr/${_triple})
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)

find_program(WINE_EXECUTABLE NAMES wine64 wine)
if(WINE_EXECUTABLE)
  set(CMAKE_CROSSCOMPILING_EMULATOR "${WINE_EXECUTABLE}")
endif()
