#include "settings.hpp"

#include <charconv>
#include <cmath>
#include <string_view>

#include "platform.hpp"

namespace pt {
namespace {

std::string_view trim(std::string_view s) {
    while (!s.empty() && (s.front() == ' ' || s.front() == '\t')) s.remove_prefix(1);
    while (!s.empty() && (s.back() == ' ' || s.back() == '\t' || s.back() == '\r')) s.remove_suffix(1);
    return s;
}

template <typename T>
bool parse_num(std::string_view v, T& out) {
    T tmp{};
    auto r = std::from_chars(v.data(), v.data() + v.size(), tmp);
    if (r.ec != std::errc() || r.ptr != v.data() + v.size()) return false;
    out = tmp;
    return true;
}

}  // namespace

Settings parse_settings(const std::string& text) {
    Settings s;
    std::string_view rest(text);
    while (!rest.empty()) {
        auto nl = rest.find('\n');
        std::string_view line = rest.substr(0, nl);
        rest = nl == std::string_view::npos ? std::string_view{} : rest.substr(nl + 1);

        auto hash = line.find_first_of("#;");
        if (hash != std::string_view::npos) line = line.substr(0, hash);
        auto eq = line.find('=');
        if (eq == std::string_view::npos) continue;
        std::string_view key = trim(line.substr(0, eq));
        std::string_view val = trim(line.substr(eq + 1));

        if (key == "port") {
            std::uint32_t p = 0;
            if (parse_num(val, p) && p > 0 && p <= 65535) s.port = std::uint16_t(p);
        } else if (key == "sample_rate_hz") {
            double d = 0;
            if (parse_num(val, d) && std::isfinite(d) && d >= 0.0 && d <= 1000.0) s.sample_rate_hz = d;
        } else if (key == "replay_buffer_s") {
            double d = 0;
            if (parse_num(val, d) && std::isfinite(d) && d >= 0.0 && d <= 3600.0) s.replay_buffer_s = d;
        } else if (key == "capture") {
            s.capture = (val == "1" || val == "true");
        } else if (key == "journal_max_mib") {
            std::uint32_t m = 0;
            if (parse_num(val, m) && m >= 1 && m <= 4096) s.journal_max_mib = m;
        }
    }
    return s;
}

Settings load_settings(const std::string& ini_path) {
    std::string text;
    if (std::FILE* f = open_file_utf8(ini_path, "rb")) {
        char buf[4096];
        std::size_t n;
        while ((n = std::fread(buf, 1, sizeof buf, f)) > 0 && text.size() < 65536) text.append(buf, n);
        std::fclose(f);
    }
    return parse_settings(text);
}

}  // namespace pt
