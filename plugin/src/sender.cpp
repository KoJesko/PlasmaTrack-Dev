#include "sender.hpp"

#include <algorithm>
#include <chrono>
#include <vector>

#include "sha256.hpp"

namespace pt {
namespace {

constexpr std::size_t kOutbufLimit = 32u << 20;   // drop a client that cannot keep up
constexpr std::uint32_t kMaxInboundFrame = 1u << 20;
constexpr std::int64_t kConnectTimeoutMs = 2000;
constexpr std::int64_t kMaxBackoffMs = 5000;
constexpr std::int64_t kModuleScanMs = 10000;

// Extracts a string field ("key":"value") from one of the client's messages.
std::string json_str_field(const std::string& json, const std::string& key) {
    const std::string k = "\"" + key + "\":\"";
    auto p = json.find(k);
    if (p == std::string::npos) return {};
    p += k.size();
    auto e = json.find('"', p);
    return e == std::string::npos ? std::string{} : json.substr(p, e - p);
}

}  // namespace

Sender::Sender(SenderConfig cfg, SpscRing<Msg>& in, SpscRing<std::string>& log_out, SharedCounters& counters)
    : cfg_(std::move(cfg)), in_(in), log_out_(log_out), counters_(counters), sock_(net::invalid_socket()) {}

Sender::~Sender() { stop_and_join(); }

void Sender::start() { thread_ = std::thread([this] { run(); }); }

void Sender::stop_and_join() {
    stop_.store(true, std::memory_order_release);
    if (thread_.joinable()) thread_.join();
}

void Sender::log(const std::string& line) {
    std::string copy = "[plasmatrack] " + line;
    if (!log_out_.try_push(std::move(copy))) counters_.log_dropped.fetch_add(1, std::memory_order_relaxed);
}

void Sender::run() {
    net::startup();
    journal_ = std::make_unique<Journal>(join_path(cfg_.data_dir, "journal"), cfg_.sess,
                                         std::uint64_t(cfg_.settings.journal_max_mib) << 20);
    if (cfg_.settings.capture) {
        const std::string dir = join_path(cfg_.data_dir, "captures");
        make_dirs(dir);
        capture_ = open_file_utf8(join_path(dir, cfg_.sess + ".jsonl"), "ab");
        log(capture_ ? "capture mode: writing " + join_path(dir, cfg_.sess + ".jsonl") : "capture mode: cannot open file");
    }

    emit_own(Kind::Integrity, [this](JsonWriter& w) {
        w.key("plugin_path").str(cfg_.plugin_path);
        w.key("plugin_sha256").str(sha256_file(cfg_.plugin_path));
    });
    scan_modules(true);

    Msg m;
    for (;;) {
        bool drained_any = false;
        for (int i = 0; i < 4096 && in_.try_pop(m); ++i) {
            handle(std::move(m));
            drained_any = true;
        }
        if (stop_.load(std::memory_order_acquire) && !drained_any) break;

        if (steady_ms() >= next_module_scan_ms_) scan_modules(false);
        manage_connection();
    }

    // Best effort: give a connected client a moment to receive the tail
    // (it is journaled anyway, so nothing durable is lost if this fails).
    if (conn_ == Conn::Connected) {
        const std::int64_t until = steady_ms() + 200;
        while (out_off_ < outbuf_.size() && steady_ms() < until) {
            auto r = net::poll(sock_, true, 20);
            if (r.error) break;
            if (r.writable) {
                long n = net::send_some(sock_, outbuf_.data() + out_off_, outbuf_.size() - out_off_);
                if (n < -1) break;
                if (n > 0) out_off_ += std::size_t(n);
            }
        }
    }
    net::close(sock_);
    sock_ = net::invalid_socket();
    if (capture_) std::fclose(capture_);
    capture_ = nullptr;
    journal_.reset();
    net::cleanup();
}

void Sender::emit_own(Kind kind, const std::function<void(JsonWriter&)>& fields) {
    Msg m;
    m.kind = kind;
    m.seq = counters_.next_seq.fetch_add(1, std::memory_order_relaxed);
    JsonWriter w;
    envelope(w, kind, cfg_.sess, m.seq);
    fields(w);
    w.end_object();
    m.json = w.take();
    handle(std::move(m));
}

void Sender::scan_modules(bool force) {
    next_module_scan_ms_ = steady_ms() + kModuleScanMs;
    auto names = loaded_module_names();
    std::sort(names.begin(), names.end());
    JsonWriter list;
    list.begin_array();
    for (const auto& n : names) list.str(n);
    list.end_array();
    std::string rendered = list.take();
    if (!force && rendered == last_modules_) return;
    last_modules_ = rendered;
    emit_own(Kind::Modules, [this](JsonWriter& w) { w.key("modules").raw(last_modules_); });
}

void Sender::handle(Msg&& m) {
    if (capture_) {
        std::fwrite(m.json.data(), 1, m.json.size(), capture_);
        std::fputc('\n', capture_);
    }
    switch (m.kind) {
        case Kind::Config: latest_config_[m.config_id] = m.json; break;
        case Kind::Paused: paused_ = true; break;
        case Kind::Started: paused_ = false; break;
        case Kind::Sample: {
            last_sample_ = m.json;
            const std::int64_t now = steady_ms();
            replay_.push_back({m.seq, now, m.json});
            const auto keep_ms = std::int64_t(cfg_.settings.replay_buffer_s * 1000.0);
            while (!replay_.empty() && now - replay_.front().wall > keep_ms) replay_.pop_front();
            break;
        }
        default: break;
    }
    if (is_durable(m.kind)) journal_->append(m.seq, m.json);
    if (conn_ == Conn::Connected) enqueue(m.json);
}

void Sender::enqueue(const std::string& json) {
    if (conn_ != Conn::Connected) return;
    append_frame(outbuf_, json);
    if (outbuf_.size() - out_off_ > kOutbufLimit) disconnect("client too slow; reconnect will replay");
}

std::string Sender::build_hello() const {
    JsonWriter w;
    w.begin_object();
    w.key("v").i64(kProtocolVersion);
    w.key("kind").str("hello");
    w.key("sess").str(cfg_.sess);
    w.key("wall_ms").i64(wall_ms());
    w.key("plugin_version").str(kPluginVersion);
    w.key("last_seq").u64(counters_.next_seq.load() - 1);
    w.key("sample_rate_hz").f64(cfg_.settings.sample_rate_hz);
    w.key("replay_buffer_s").f64(cfg_.settings.replay_buffer_s);
    w.key("capture").boolean(cfg_.settings.capture);
    w.key("ring_dropped").u64(counters_.ring_dropped.load());
    w.key("journal_dropped").u64(journal_->dropped());
    w.key("log_dropped").u64(counters_.log_dropped.load());
    w.end_object();
    return w.take();
}

std::string Sender::build_snapshot() const {
    JsonWriter w;
    w.begin_object();
    w.key("v").i64(kProtocolVersion);
    w.key("kind").str("snapshot");
    w.key("sess").str(cfg_.sess);
    w.key("wall_ms").i64(wall_ms());
    w.key("paused").boolean(paused_);
    w.key("configs").begin_array();
    for (const auto& [id, json] : latest_config_) w.raw(json);
    w.end_array();
    w.key("last_sample");
    if (last_sample_.empty()) w.null(); else w.raw(last_sample_);
    w.end_object();
    return w.take();
}

void Sender::on_connected() {
    conn_ = Conn::Connected;
    backoff_ms_ = 250;
    outbuf_.clear();
    out_off_ = 0;
    inbuf_.clear();
    log("connected to client on 127.0.0.1:" + std::to_string(cfg_.settings.port));

    enqueue(build_hello());
    enqueue(build_snapshot());

    // Replay: unacknowledged durable messages of earlier sessions first, then
    // this session's durable messages and buffered samples merged by seq.
    // The client de-duplicates by (sess, seq).
    for (const auto& [sess, entries] : journal_->pending()) {
        if (sess == cfg_.sess) continue;
        for (const auto& e : entries) enqueue(e.json);
    }
    struct Item { std::uint64_t seq; const std::string* json; };
    std::vector<Item> items;
    if (auto it = journal_->pending().find(cfg_.sess); it != journal_->pending().end()) {
        for (const auto& e : it->second) items.push_back({e.seq, &e.json});
    }
    for (const auto& s : replay_) items.push_back({s.seq, &s.json});
    std::sort(items.begin(), items.end(), [](const Item& a, const Item& b) { return a.seq < b.seq; });
    for (const auto& i : items) enqueue(*i.json);
}

void Sender::disconnect(const char* why) {
    if (conn_ == Conn::Connected) log(std::string("disconnected: ") + why);
    net::close(sock_);
    sock_ = net::invalid_socket();
    conn_ = Conn::Disconnected;
    outbuf_.clear();
    out_off_ = 0;
    next_attempt_ms_ = steady_ms() + backoff_ms_;
    backoff_ms_ = std::min(backoff_ms_ * 2, kMaxBackoffMs);
}

void Sender::read_acks() {
    char buf[8192];
    for (;;) {
        long n = net::recv_some(sock_, buf, sizeof buf);
        if (n == -1) break;
        if (n < 0) { disconnect("connection closed"); return; }
        inbuf_.append(buf, std::size_t(n));
    }
    std::size_t off = 0;
    while (inbuf_.size() - off >= 4) {
        const auto* p = reinterpret_cast<const unsigned char*>(inbuf_.data() + off);
        const std::uint32_t len = std::uint32_t(p[0]) | (std::uint32_t(p[1]) << 8) | (std::uint32_t(p[2]) << 16) |
                                  (std::uint32_t(p[3]) << 24);
        if (len > kMaxInboundFrame) { disconnect("oversized frame from client"); return; }
        if (inbuf_.size() - off - 4 < len) break;
        const std::string msg = inbuf_.substr(off + 4, len);
        off += 4 + len;
        if (json_str_field(msg, "kind") == "ack") {
            const std::string sess = json_str_field(msg, "sess");
            const std::uint64_t seq = Journal::seq_of(msg);
            journal_->ack(sess, seq);
            if (sess == cfg_.sess) {
                while (!replay_.empty() && replay_.front().seq <= seq) replay_.pop_front();
            }
        }
    }
    inbuf_.erase(0, off);
}

void Sender::manage_connection() {
    const std::int64_t now = steady_ms();
    switch (conn_) {
        case Conn::Disconnected: {
            if (now < next_attempt_ms_) {
                std::this_thread::sleep_for(std::chrono::milliseconds(20));
                return;
            }
            bool pending = false;
            sock_ = net::connect_loopback(cfg_.settings.port, pending);
            if (sock_ == net::invalid_socket()) { disconnect("connect failed"); return; }
            if (!pending) { on_connected(); return; }
            conn_ = Conn::Connecting;
            connect_started_ms_ = now;
            return;
        }
        case Conn::Connecting: {
            auto r = net::poll(sock_, true, 20);
            if (r.error || (r.writable && !net::connect_succeeded(sock_))) { disconnect("no client listening"); return; }
            if (r.writable) { on_connected(); return; }
            if (now - connect_started_ms_ > kConnectTimeoutMs) disconnect("connect timeout");
            return;
        }
        case Conn::Connected: {
            auto r = net::poll(sock_, out_off_ < outbuf_.size(), 20);
            if (r.error) { disconnect("socket error"); return; }
            if (r.readable) {
                read_acks();
                if (conn_ != Conn::Connected) return;
            }
            if (r.writable && out_off_ < outbuf_.size()) {
                long n = net::send_some(sock_, outbuf_.data() + out_off_, outbuf_.size() - out_off_);
                if (n < -1) { disconnect("send failed"); return; }
                if (n > 0) out_off_ += std::size_t(n);
                if (out_off_ == outbuf_.size()) { outbuf_.clear(); out_off_ = 0; }
            }
            return;
        }
    }
}

}  // namespace pt
