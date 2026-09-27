// On-disk journal of durable messages (init, config, gameplay, paused/started,
// timer_restart, integrity, modules, shutdown) until the client acknowledges
// them. A client that was closed during a delivery still receives it when it
// next connects, even after the game has restarted.
//
// Layout in `dir`: `<sess>.jsonl` (one message per line, appended) and
// `<sess>.ack` (highest acknowledged seq, decimal). Only the sender thread
// touches the journal.
#pragma once

#include <cstdint>
#include <deque>
#include <map>
#include <string>

namespace pt {

struct JournalEntry {
    std::uint64_t seq;
    std::string json;
};

class Journal {
public:
    Journal(std::string dir, std::string current_sess, std::uint64_t max_bytes);

    // Appends a durable message of the current session. Returns false (and the
    // message is still kept in memory for this run) if the disk write failed or
    // the size cap was reached.
    bool append(std::uint64_t seq, const std::string& json);

    // Everything <= seq of `sess` is acknowledged by the client.
    void ack(const std::string& sess, std::uint64_t seq);

    // Unacknowledged entries, per session, oldest session first is not
    // guaranteed; each deque is in seq order.
    const std::map<std::string, std::deque<JournalEntry>>& pending() const { return pending_; }

    std::uint64_t dropped() const { return dropped_; }

    // Parses the `"seq":N` field of one of our own messages; 0 if absent.
    static std::uint64_t seq_of(const std::string& json);

private:
    std::string file_for(const std::string& sess, const char* ext) const;
    void load_previous_sessions();
    void compact_current();

    std::string dir_;
    std::string sess_;
    std::uint64_t max_bytes_;
    std::uint64_t written_bytes_ = 0;
    std::uint64_t dropped_ = 0;
    std::map<std::string, std::deque<JournalEntry>> pending_;
};

}  // namespace pt
