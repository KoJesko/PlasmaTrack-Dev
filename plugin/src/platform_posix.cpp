// POSIX implementation (native Linux build; secondary target).
#ifndef _WIN32

#include <arpa/inet.h>
#include <dirent.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cstring>
#include <set>

#include "platform.hpp"

namespace pt {

std::FILE* open_file_utf8(const std::string& path, const char* mode) { return std::fopen(path.c_str(), mode); }

bool make_dirs(const std::string& path) {
    if (path.empty()) return false;
    for (std::size_t i = 1; i <= path.size(); ++i) {
        if (i == path.size() || path[i] == '/') {
            std::string part = path.substr(0, i);
            if (::mkdir(part.c_str(), 0755) != 0 && errno != EEXIST) return false;
        }
    }
    return true;
}

bool remove_file(const std::string& path) { return ::unlink(path.c_str()) == 0; }
bool replace_file(const std::string& from, const std::string& to) { return ::rename(from.c_str(), to.c_str()) == 0; }

std::vector<std::string> list_dir(const std::string& dir) {
    std::vector<std::string> out;
    DIR* d = ::opendir(dir.c_str());
    if (!d) return out;
    while (dirent* e = ::readdir(d)) {
        std::string name = e->d_name;
        if (name == "." || name == "..") continue;
        struct stat st;
        if (::stat(join_path(dir, name).c_str(), &st) == 0 && S_ISREG(st.st_mode)) out.push_back(name);
    }
    ::closedir(d);
    return out;
}

std::string dir_of(const std::string& path) {
    auto p = path.find_last_of('/');
    return p == std::string::npos ? std::string(".") : path.substr(0, p);
}

std::string join_path(const std::string& dir, const std::string& name) { return dir + "/" + name; }

std::string own_module_path() {
    Dl_info info{};
    if (::dladdr(reinterpret_cast<void*>(&own_module_path), &info) == 0 || !info.dli_fname) return {};
    return info.dli_fname;
}

std::vector<std::string> loaded_module_names() {
    std::set<std::string> names;
    std::FILE* f = std::fopen("/proc/self/maps", "r");
    if (!f) return {};
    char line[4096];
    while (std::fgets(line, sizeof line, f)) {
        const char* slash = std::strchr(line, '/');
        if (!slash) continue;
        std::string path(slash);
        while (!path.empty() && (path.back() == '\n' || path.back() == ' ')) path.pop_back();
        auto p = path.find_last_of('/');
        names.insert(p == std::string::npos ? path : path.substr(p + 1));
    }
    std::fclose(f);
    return {names.begin(), names.end()};
}

std::uint64_t os_random_u64() {
    std::uint64_t v = 0;
    if (std::FILE* f = std::fopen("/dev/urandom", "rb")) {
        if (std::fread(&v, sizeof v, 1, f) != 1) v = 0;
        std::fclose(f);
    }
    return v;
}

namespace net {

Socket invalid_socket() { return -1; }
bool startup() { return true; }
void cleanup() {}

Socket connect_loopback(std::uint16_t port, bool& pending) {
    pending = false;
    int s = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (s < 0) return -1;
    ::fcntl(s, F_SETFL, ::fcntl(s, F_GETFL, 0) | O_NONBLOCK);
    ::fcntl(s, F_SETFD, FD_CLOEXEC);
    int one = 1;
    ::setsockopt(s, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
#ifdef SO_NOSIGPIPE
    ::setsockopt(s, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
#endif
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (::connect(s, reinterpret_cast<sockaddr*>(&addr), sizeof addr) == 0) return s;
    if (errno == EINPROGRESS) { pending = true; return s; }
    ::close(s);
    return -1;
}

PollResult poll(Socket s, bool want_write, int timeout_ms) {
    PollResult r;
    fd_set rs, ws;
    FD_ZERO(&rs); FD_ZERO(&ws);
    FD_SET(s, &rs);
    if (want_write) FD_SET(s, &ws);
    timeval tv{timeout_ms / 1000, (timeout_ms % 1000) * 1000};
    int n = ::select(s + 1, &rs, want_write ? &ws : nullptr, nullptr, &tv);
    if (n < 0) { r.error = errno != EINTR; return r; }
    r.readable = FD_ISSET(s, &rs);
    r.writable = want_write && FD_ISSET(s, &ws);
    return r;
}

bool connect_succeeded(Socket s) {
    int err = 0;
    socklen_t len = sizeof err;
    return ::getsockopt(s, SOL_SOCKET, SO_ERROR, &err, &len) == 0 && err == 0;
}

long send_some(Socket s, const char* data, std::size_t len) {
#ifdef MSG_NOSIGNAL
    ssize_t n = ::send(s, data, len, MSG_NOSIGNAL);
#else
    ssize_t n = ::send(s, data, len, 0);
#endif
    if (n >= 0) return long(n);
    return (errno == EAGAIN || errno == EWOULDBLOCK) ? -1 : -2;
}

long recv_some(Socket s, char* data, std::size_t len) {
    ssize_t n = ::recv(s, data, len, 0);
    if (n > 0) return long(n);
    if (n == 0) return -2;
    return (errno == EAGAIN || errno == EWOULDBLOCK) ? -1 : -2;
}

void close(Socket s) {
    if (s >= 0) ::close(s);
}

}  // namespace net
}  // namespace pt

#endif  // !_WIN32
