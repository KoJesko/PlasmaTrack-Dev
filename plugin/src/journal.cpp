#include "journal.hpp"

#include <charconv>
#include <cstdio>

#include "platform.hpp"

namespace pt {
namespace {

bool ends_with(const std::string& s, const std::string& suffix) {
    return s.size() >= suffix.size() && s.compare(s.size() - suffix.size(), suffix.size(), suffix) == 0;
}

std::uint64_t read_ack(const std::string& path) {
    std::uint64_t v = 0;
    if (std::FILE* f = open_file_utf8(path, "rb")) {
        char buf[32] = {};
        std::size_t n = std::fread(buf, 1, sizeof buf - 1, f);
        std::fclose(f);
        std::from_chars(buf, buf + n, v);
    }
    return v;
}

void write_ack(const std::string& path, std::uint64_t seq) {
    const std::string tmp = path + ".tmp";
    if (std::FILE* f = open_file_utf8(tmp, "wb")) {
        char buf[32];
        auto r = std::to_chars(buf, buf + sizeof buf, seq);
        std::fwrite(buf, 1, std::size_t(r.ptr - buf), f);
        std::fclose(f);
        replace_file(tmp, path);
    }
}

}  // namespace

std::uint64_t Journal::seq_of(const std::string& json) {
    static const std::string key = "\"seq\":";
    auto p = json.find(key);
    if (p == std::string::npos) return 0;
    std::uint64_t v = 0;
    const char* b = json.data() + p + key.size();
    std::from_chars(b, json.data() + json.size(), v);
    return v;
}

Journal::Journal(std::string dir, std::string current_sess, std::uint64_t max_bytes)
    : dir_(std::move(dir)), sess_(std::move(current_sess)), max_bytes_(max_bytes) {
    make_dirs(dir_);
    load_previous_sessions();
}

std::string Journal::file_for(const std::string& sess, const char* ext) const {
    return join_path(dir_, sess + ext);
}

void Journal::load_previous_sessions() {
    for (const std::string& name : list_dir(dir_)) {
        if (!ends_with(name, ".jsonl")) continue;
        const std::string sess = name.substr(0, name.size() - 6);
        if (sess == sess_) continue;
        const std::uint64_t acked = read_ack(file_for(sess, ".ack"));
        std::deque<JournalEntry> entries;
        if (std::FILE* f = open_file_utf8(file_for(sess, ".jsonl"), "rb")) {
            std::string line;
            int c;
            while ((c = std::fgetc(f)) != EOF) {
                if (c != '\n') { line += char(c); continue; }
                const std::uint64_t seq = seq_of(line);
                if (seq > acked) entries.push_back({seq, line});
                line.clear();
            }
            std::fclose(f);  // a trailing partial line (crash mid-write) is discarded
        }
        if (entries.empty()) {
            remove_file(file_for(sess, ".jsonl"));
            remove_file(file_for(sess, ".ack"));
        } else {
            pending_[sess] = std::move(entries);
        }
    }
}

bool Journal::append(std::uint64_t seq, const std::string& json) {
    pending_[sess_].push_back({seq, json});
    if (written_bytes_ + json.size() + 1 > max_bytes_) {
        ++dropped_;
        return false;
    }
    std::FILE* f = open_file_utf8(file_for(sess_, ".jsonl"), "ab");
    if (!f) { ++dropped_; return false; }
    const bool ok = std::fwrite(json.data(), 1, json.size(), f) == json.size() && std::fputc('\n', f) != EOF;
    std::fclose(f);
    written_bytes_ += json.size() + 1;
    if (!ok) ++dropped_;
    return ok;
}

void Journal::ack(const std::string& sess, std::uint64_t seq) {
    auto it = pending_.find(sess);
    if (it == pending_.end()) return;
    auto& q = it->second;
    while (!q.empty() && q.front().seq <= seq) q.pop_front();
    write_ack(file_for(sess, ".ack"), seq);
    if (sess == sess_) {
        compact_current();
    } else if (q.empty()) {
        remove_file(file_for(sess, ".jsonl"));
        remove_file(file_for(sess, ".ack"));
        pending_.erase(it);
    }
}

void Journal::compact_current() {
    // Once everything of the current session is acknowledged, restart its file
    // so it does not grow for the whole game session.
    auto it = pending_.find(sess_);
    if (it != pending_.end() && it->second.empty() && written_bytes_ > (1u << 20)) {
        if (std::FILE* f = open_file_utf8(file_for(sess_, ".jsonl"), "wb")) std::fclose(f);
        written_bytes_ = 0;
    }
}

}  // namespace pt
