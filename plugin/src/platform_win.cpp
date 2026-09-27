// Windows implementation (also what runs under Proton/Wine).
#ifdef _WIN32

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <bcrypt.h>
#include <psapi.h>

#include "platform.hpp"

namespace pt {
namespace {

std::wstring widen(const std::string& s) {
    if (s.empty()) return {};
    int n = MultiByteToWideChar(CP_UTF8, 0, s.data(), int(s.size()), nullptr, 0);
    std::wstring w(std::size_t(n), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.data(), int(s.size()), w.data(), n);
    return w;
}

std::string narrow(const wchar_t* w, std::size_t len) {
    if (len == 0) return {};
    int n = WideCharToMultiByte(CP_UTF8, 0, w, int(len), nullptr, 0, nullptr, nullptr);
    std::string s(std::size_t(n), '\0');
    WideCharToMultiByte(CP_UTF8, 0, w, int(len), s.data(), n, nullptr, nullptr);
    return s;
}

}  // namespace

std::FILE* open_file_utf8(const std::string& path, const char* mode) {
    return _wfopen(widen(path).c_str(), widen(mode).c_str());
}

bool make_dirs(const std::string& path) {
    if (path.empty()) return false;
    std::wstring w = widen(path);
    for (std::size_t i = 1; i <= w.size(); ++i) {
        if (i == w.size() || w[i] == L'\\' || w[i] == L'/') {
            std::wstring part = w.substr(0, i);
            if (part.size() == 2 && part[1] == L':') continue;  // drive letter
            if (!CreateDirectoryW(part.c_str(), nullptr) && GetLastError() != ERROR_ALREADY_EXISTS) return false;
        }
    }
    return true;
}

bool remove_file(const std::string& path) { return DeleteFileW(widen(path).c_str()) != 0; }

bool replace_file(const std::string& from, const std::string& to) {
    return MoveFileExW(widen(from).c_str(), widen(to).c_str(), MOVEFILE_REPLACE_EXISTING) != 0;
}

std::vector<std::string> list_dir(const std::string& dir) {
    std::vector<std::string> out;
    WIN32_FIND_DATAW fd;
    HANDLE h = FindFirstFileW(widen(join_path(dir, "*")).c_str(), &fd);
    if (h == INVALID_HANDLE_VALUE) return out;
    do {
        if (!(fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)) out.push_back(narrow(fd.cFileName, wcslen(fd.cFileName)));
    } while (FindNextFileW(h, &fd));
    FindClose(h);
    return out;
}

std::string dir_of(const std::string& path) {
    auto p = path.find_last_of("\\/");
    return p == std::string::npos ? std::string(".") : path.substr(0, p);
}

std::string join_path(const std::string& dir, const std::string& name) { return dir + "\\" + name; }

std::string own_module_path() {
    HMODULE self = nullptr;
    if (!GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                            reinterpret_cast<LPCWSTR>(&own_module_path), &self)) {
        return {};
    }
    std::wstring buf(1024, L'\0');
    for (;;) {
        DWORD n = GetModuleFileNameW(self, buf.data(), DWORD(buf.size()));
        if (n == 0) return {};
        if (n < buf.size()) return narrow(buf.data(), n);
        buf.resize(buf.size() * 2);
    }
}

std::vector<std::string> loaded_module_names() {
    std::vector<std::string> out;
    HANDLE proc = GetCurrentProcess();
    std::vector<HMODULE> mods(512);
    DWORD needed = 0;
    for (;;) {
        if (!K32EnumProcessModules(proc, mods.data(), DWORD(mods.size() * sizeof(HMODULE)), &needed)) return out;
        if (needed <= mods.size() * sizeof(HMODULE)) break;
        mods.resize(needed / sizeof(HMODULE) + 16);
    }
    const std::size_t count = needed / sizeof(HMODULE);
    wchar_t name[MAX_PATH];
    for (std::size_t i = 0; i < count; ++i) {
        DWORD n = K32GetModuleBaseNameW(proc, mods[i], name, MAX_PATH);
        if (n > 0) out.push_back(narrow(name, n));
    }
    return out;
}

std::uint64_t os_random_u64() {
    std::uint64_t v = 0;
    BCryptGenRandom(nullptr, reinterpret_cast<PUCHAR>(&v), sizeof v, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    return v;
}

namespace net {

Socket invalid_socket() { return INVALID_SOCKET; }

bool startup() {
    WSADATA wsa;
    return WSAStartup(MAKEWORD(2, 2), &wsa) == 0;
}

void cleanup() { WSACleanup(); }

Socket connect_loopback(std::uint16_t port, bool& pending) {
    pending = false;
    SOCKET s = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (s == INVALID_SOCKET) return INVALID_SOCKET;
    u_long nb = 1;
    ioctlsocket(s, FIONBIO, &nb);
    int one = 1;
    setsockopt(s, IPPROTO_TCP, TCP_NODELAY, reinterpret_cast<const char*>(&one), sizeof one);
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);  // literal 127.0.0.1, never "localhost"
    if (::connect(s, reinterpret_cast<sockaddr*>(&addr), sizeof addr) == 0) return s;
    if (WSAGetLastError() == WSAEWOULDBLOCK) { pending = true; return s; }
    closesocket(s);
    return INVALID_SOCKET;
}

PollResult poll(Socket s, bool want_write, int timeout_ms) {
    PollResult r;
    fd_set rs, ws, es;
    FD_ZERO(&rs); FD_ZERO(&ws); FD_ZERO(&es);
    FD_SET(SOCKET(s), &rs);
    if (want_write) FD_SET(SOCKET(s), &ws);
    FD_SET(SOCKET(s), &es);
    timeval tv{timeout_ms / 1000, (timeout_ms % 1000) * 1000};
    int n = ::select(0, &rs, want_write ? &ws : nullptr, &es, &tv);
    if (n == SOCKET_ERROR) { r.error = true; return r; }
    r.readable = FD_ISSET(SOCKET(s), &rs) != 0;
    r.writable = want_write && FD_ISSET(SOCKET(s), &ws) != 0;
    r.error = FD_ISSET(SOCKET(s), &es) != 0;  // on Windows a failed connect is reported here
    return r;
}

bool connect_succeeded(Socket s) {
    int err = 0;
    int len = sizeof err;
    if (getsockopt(SOCKET(s), SOL_SOCKET, SO_ERROR, reinterpret_cast<char*>(&err), &len) != 0) return false;
    return err == 0;
}

long send_some(Socket s, const char* data, std::size_t len) {
    int n = ::send(SOCKET(s), data, int(len > 1 << 20 ? 1 << 20 : len), 0);
    if (n >= 0) return n;
    return WSAGetLastError() == WSAEWOULDBLOCK ? -1 : -2;
}

long recv_some(Socket s, char* data, std::size_t len) {
    int n = ::recv(SOCKET(s), data, int(len), 0);
    if (n > 0) return n;
    if (n == 0) return -2;  // orderly close
    return WSAGetLastError() == WSAEWOULDBLOCK ? -1 : -2;
}

void close(Socket s) {
    if (s != INVALID_SOCKET) closesocket(SOCKET(s));
}

}  // namespace net
}  // namespace pt

#endif  // _WIN32
