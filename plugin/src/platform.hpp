// OS abstraction for the plugin: files, module paths, randomness and sockets.
// Windows (native and under Proton/Wine) is primary; POSIX is the native-Linux
// secondary target.
#pragma once

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

namespace pt {

// ---- files -----------------------------------------------------------------
std::FILE* open_file_utf8(const std::string& path, const char* mode);
bool make_dirs(const std::string& path);
bool remove_file(const std::string& path);
bool replace_file(const std::string& from, const std::string& to);
std::vector<std::string> list_dir(const std::string& dir);  // plain file names
std::string dir_of(const std::string& path);
std::string join_path(const std::string& dir, const std::string& name);

// ---- process ---------------------------------------------------------------
// UTF-8 path of the plugin module itself (DLL or .so).
std::string own_module_path();
// Base file names of all modules loaded in this process (reported raw; the
// client decides what they mean, e.g. TruckersMP detection).
std::vector<std::string> loaded_module_names();
// Cryptographically random 64-bit value from the OS.
std::uint64_t os_random_u64();

// Monotonic milliseconds for timers (immune to wall-clock changes).
inline std::int64_t steady_ms() {
    using namespace std::chrono;
    return duration_cast<milliseconds>(steady_clock::now().time_since_epoch()).count();
}

inline std::int64_t wall_ms() {
    using namespace std::chrono;
    return duration_cast<milliseconds>(system_clock::now().time_since_epoch()).count();
}

// ---- sockets (IPv4 loopback only) ------------------------------------------
namespace net {

#ifdef _WIN32
using Socket = std::uintptr_t;
#else
using Socket = int;
#endif

Socket invalid_socket();
bool startup();   // WSAStartup on Windows; no-op elsewhere
void cleanup();

// Starts a non-blocking connect to 127.0.0.1:port. Returns invalid_socket() on
// immediate failure; `pending` is set when the connect is still in progress.
Socket connect_loopback(std::uint16_t port, bool& pending);

struct PollResult { bool readable = false; bool writable = false; bool error = false; };
PollResult poll(Socket s, bool want_write, int timeout_ms);

// True once a pending connect completed successfully.
bool connect_succeeded(Socket s);

// >= 0: bytes transferred. -1: would block. -2: connection error or closed.
long send_some(Socket s, const char* data, std::size_t len);
long recv_some(Socket s, char* data, std::size_t len);

void close(Socket s);

}  // namespace net
}  // namespace pt
