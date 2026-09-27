// The sender thread. It is the only owner of the socket and of the journal, and
// it never calls into the SDK (SDK readme: all calls back to the game must
// happen on the main thread); log lines go back to the game thread through
// `log_out` and are flushed from a telemetry callback.
#pragma once

#include <atomic>
#include <cstdint>
#include <deque>
#include <functional>
#include <map>
#include <memory>
#include <string>
#include <thread>

#include "journal.hpp"
#include "message.hpp"
#include "platform.hpp"
#include "settings.hpp"
#include "spsc_ring.hpp"

namespace pt {

struct SharedCounters {
    std::atomic<std::uint64_t> next_seq{1};
    std::atomic<std::uint64_t> ring_dropped{0};
    std::atomic<std::uint64_t> log_dropped{0};
};

struct SenderConfig {
    Settings settings;
    std::string sess;
    std::string plugin_path;  // for the integrity self-hash
    std::string data_dir;     // <plugin dir>/plasmatrack
};

class Sender {
public:
    Sender(SenderConfig cfg, SpscRing<Msg>& in, SpscRing<std::string>& log_out, SharedCounters& counters);
    ~Sender();

    void start();
    // Signals the thread and joins it without a timeout: the DLL may be
    // unloaded right after scs_telemetry_shutdown returns.
    void stop_and_join();

private:
    enum class Conn { Disconnected, Connecting, Connected };

    void run();
    void handle(Msg&& m);
    // Emits a sender-originated durable message; `fields` writes extra keys.
    void emit_own(Kind kind, const std::function<void(JsonWriter&)>& fields);
    void scan_modules(bool force);
    void manage_connection();
    void on_connected();
    void disconnect(const char* why);
    void enqueue(const std::string& json);
    void read_acks();
    void log(const std::string& line);
    std::string build_hello() const;
    std::string build_snapshot() const;

    SenderConfig cfg_;
    SpscRing<Msg>& in_;
    SpscRing<std::string>& log_out_;
    SharedCounters& counters_;
    std::atomic<bool> stop_{false};
    std::thread thread_;

    std::unique_ptr<Journal> journal_;
    std::FILE* capture_ = nullptr;

    // Snapshot state reconstructed from the message stream.
    std::map<std::string, std::string> latest_config_;  // config id -> config message
    bool paused_ = true;  // telemetry starts paused (scssdk_telemetry.h:87)
    std::string last_sample_;
    std::string last_modules_;

    struct Sampled { std::uint64_t seq; std::int64_t wall; std::string json; };
    std::deque<Sampled> replay_;

    Conn conn_ = Conn::Disconnected;
    net::Socket sock_;
    std::int64_t connect_started_ms_ = 0;
    std::int64_t next_attempt_ms_ = 0;
    std::int64_t backoff_ms_ = 250;
    std::string outbuf_;
    std::size_t out_off_ = 0;
    std::string inbuf_;
    std::int64_t next_module_scan_ms_ = 0;
};

}  // namespace pt
