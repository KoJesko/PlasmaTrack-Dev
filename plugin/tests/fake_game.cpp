// Fake-game harness: loads the real plugin module and drives it through the SDK
// callbacks the way the game does, while acting as the PlasmaTrack client on a
// real 127.0.0.1 socket.
//
// Usage: pt_fake_game <path to plasmatrack.dll / plasmatrack.so>
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <functional>
#include <map>
#include <set>
#include <string>
#include <thread>
#include <vector>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#else
#include <arpa/inet.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

#include "scssdk_telemetry.h"
#include "common/scssdk_telemetry_common_channels.h"
#include "common/scssdk_telemetry_common_configs.h"
#include "common/scssdk_telemetry_common_gameplay_events.h"
#include "common/scssdk_telemetry_truck_common_channels.h"
#include "eurotrucks2/scssdk_eut2.h"
#include "eurotrucks2/scssdk_telemetry_eut2.h"

#include "platform.hpp"
#include "sha256.hpp"

static int g_failures = 0;
#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                      \
        }                                                                      \
    } while (0)
#define REQUIRE(cond)                                                          \
    do {                                                                       \
        if (!(cond)) {                                                         \
            std::fprintf(stderr, "FATAL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            return 1;                                                          \
        }                                                                      \
    } while (0)

// ---- mock SDK ----------------------------------------------------------------

struct MockChannel {
    std::string name;
    scs_value_type_t type;
    scs_u32_t flags;
    scs_telemetry_channel_callback_t cb;
    scs_context_t ctx;
};

struct Mock {
    std::thread::id main_thread;
    int thread_violations = 0;
    std::map<scs_event_t, scs_telemetry_event_callback_t> events;
    std::vector<MockChannel> channels;
    std::vector<std::string> logs;
    void check_thread() {
        if (std::this_thread::get_id() != main_thread) ++thread_violations;
    }
    void reset() {
        events.clear();
        channels.clear();
    }
} g_mock;

SCSAPI_RESULT mock_register_event(const scs_event_t e, const scs_telemetry_event_callback_t cb, const scs_context_t) {
    g_mock.check_thread();
    if (g_mock.events.count(e)) return SCS_RESULT_already_registered;
    g_mock.events[e] = cb;
    return SCS_RESULT_ok;
}
SCSAPI_RESULT mock_unregister_event(const scs_event_t e) {
    g_mock.check_thread();
    g_mock.events.erase(e);
    return SCS_RESULT_ok;
}
SCSAPI_RESULT mock_register_channel(const scs_string_t name, const scs_u32_t, const scs_value_type_t type,
                                    const scs_u32_t flags, const scs_telemetry_channel_callback_t cb,
                                    const scs_context_t ctx) {
    g_mock.check_thread();
    g_mock.channels.push_back({name, type, flags, cb, ctx});
    return SCS_RESULT_ok;
}
SCSAPI_RESULT mock_unregister_channel(const scs_string_t, const scs_u32_t, const scs_value_type_t) {
    g_mock.check_thread();
    return SCS_RESULT_ok;
}
SCSAPI_VOID mock_log(const scs_log_type_t, const scs_string_t msg) {
    g_mock.check_thread();
    g_mock.logs.push_back(msg);
}

using init_fn = scs_result_t (*)(scs_u32_t, const scs_telemetry_init_params_t*);
using shutdown_fn = void (*)();

// ---- minimal loopback listener -----------------------------------------------

#ifdef _WIN32
using sock_t = SOCKET;
static const sock_t kBad = INVALID_SOCKET;
static void close_sock(sock_t s) { closesocket(s); }
static void set_nonblocking(sock_t s) { u_long nb = 1; ioctlsocket(s, FIONBIO, &nb); }
#else
using sock_t = int;
static const sock_t kBad = -1;
static void close_sock(sock_t s) { ::close(s); }
static void set_nonblocking(sock_t s) { fcntl(s, F_SETFL, fcntl(s, F_GETFL, 0) | O_NONBLOCK); }
#endif

struct Message {
    std::string kind, sess, json;
    std::uint64_t seq = 0;
};

static std::string str_field(const std::string& json, const std::string& key) {
    const std::string k = "\"" + key + "\":\"";
    auto p = json.find(k);
    if (p == std::string::npos) return {};
    p += k.size();
    return json.substr(p, json.find('"', p) - p);
}
static std::uint64_t seq_field(const std::string& json) {
    auto p = json.find("\"seq\":");
    return p == std::string::npos ? 0 : std::strtoull(json.c_str() + p + 6, nullptr, 10);
}

class Client {
public:
    sock_t listener = kBad, conn = kBad;
    std::string inbuf;
    std::vector<Message> received;

    std::uint16_t listen_any() {
        listener = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        sockaddr_in a{};
        a.sin_family = AF_INET;
        a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        a.sin_port = 0;
        ::bind(listener, reinterpret_cast<sockaddr*>(&a), sizeof a);
        ::listen(listener, 4);
        set_nonblocking(listener);
        socklen_t len = sizeof a;
        ::getsockname(listener, reinterpret_cast<sockaddr*>(&a), &len);
        return ntohs(a.sin_port);
    }

    bool accept_within(int ms) {
        const auto end = std::chrono::steady_clock::now() + std::chrono::milliseconds(ms);
        while (std::chrono::steady_clock::now() < end) {
            sock_t s = ::accept(listener, nullptr, nullptr);
            if (s != kBad) {
                set_nonblocking(s);
                conn = s;
                inbuf.clear();
                return true;
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(5));
        }
        return false;
    }

    void pump() {
        char buf[65536];
        for (;;) {
            int n = int(::recv(conn, buf, sizeof buf, 0));
            if (n <= 0) break;
            inbuf.append(buf, std::size_t(n));
        }
        std::size_t off = 0;
        while (inbuf.size() - off >= 4) {
            const auto* p = reinterpret_cast<const unsigned char*>(inbuf.data() + off);
            std::uint32_t len = p[0] | (p[1] << 8) | (p[2] << 16) | (std::uint32_t(p[3]) << 24);
            if (inbuf.size() - off - 4 < len) break;
            Message m;
            m.json = inbuf.substr(off + 4, len);
            m.kind = str_field(m.json, "kind");
            m.sess = str_field(m.json, "sess");
            m.seq = seq_field(m.json);
            received.push_back(std::move(m));
            off += 4 + len;
        }
        inbuf.erase(0, off);
    }

    // Pumps until `pred` holds over the received messages or the timeout expires.
    bool wait_for(const std::function<bool(const std::vector<Message>&)>& pred, int ms = 5000) {
        const auto end = std::chrono::steady_clock::now() + std::chrono::milliseconds(ms);
        do {
            pump();
            if (pred(received)) return true;
            std::this_thread::sleep_for(std::chrono::milliseconds(5));
        } while (std::chrono::steady_clock::now() < end);
        return false;
    }

    void send_ack(const std::string& sess, std::uint64_t seq) {
        std::string json = "{\"kind\":\"ack\",\"sess\":\"" + sess + "\",\"seq\":" + std::to_string(seq) + "}";
        std::uint32_t n = std::uint32_t(json.size());
        char hdr[4] = {char(n), char(n >> 8), char(n >> 16), char(n >> 24)};
        std::string frame(hdr, 4);
        frame += json;
        ::send(conn, frame.data(), int(frame.size()), 0);
    }

    void drop_connection() {
        close_sock(conn);
        conn = kBad;
    }
};

static bool has_kind(const std::vector<Message>& v, const std::string& kind, const std::string& sess = {}) {
    return std::any_of(v.begin(), v.end(), [&](const Message& m) {
        return m.kind == kind && (sess.empty() || m.sess == sess);
    });
}
static const Message* find_kind(const std::vector<Message>& v, const std::string& kind, const std::string& sess = {}) {
    for (const auto& m : v)
        if (m.kind == kind && (sess.empty() || m.sess == sess)) return &m;
    return nullptr;
}

// ---- driving the plugin like the game ------------------------------------------

struct Game {
    scs_timestamp_t sim_us = 0;

    void event(scs_event_t e, const void* info = nullptr) {
        auto it = g_mock.events.find(e);
        if (it != g_mock.events.end()) it->second(e, info, nullptr);
    }

    // One physics frame (SDK order: frame_start, gameplay, channels, frame_end).
    double scale = 19.0;  // local.scale reported by the next frames

    void frame(bool paused, double speed_mps, double odo_km, double fuel_l, double x,
               scs_u32_t flags = 0, bool fuel_available = true,
               const scs_telemetry_gameplay_event_t* gameplay = nullptr) {
        if (!paused) sim_us += 16666;  // the real game's physics step (observed in ATS 1.61)
        scs_telemetry_frame_start_t fs{};
        fs.flags = flags;
        fs.render_time = sim_us + 5000;
        fs.simulation_time = sim_us;
        fs.paused_simulation_time = sim_us;
        event(SCS_TELEMETRY_EVENT_frame_start, &fs);
        if (gameplay) event(SCS_TELEMETRY_EVENT_gameplay, gameplay);
        if (!paused) {
            for (auto& c : g_mock.channels) {
                scs_value_t v{};
                v.type = c.type;
                const scs_value_t* pv = &v;
                if (c.name == SCS_TELEMETRY_TRUCK_CHANNEL_speed) v.value_float.value = float(speed_mps);
                else if (c.name == SCS_TELEMETRY_TRUCK_CHANNEL_odometer) v.value_float.value = float(odo_km);
                else if (c.name == SCS_TELEMETRY_TRUCK_CHANNEL_fuel) { v.value_float.value = float(fuel_l); if (!fuel_available) pv = nullptr; }
                else if (c.name == SCS_TELEMETRY_TRUCK_CHANNEL_world_placement) v.value_dplacement.position = {x, 50.0, -1000.0};
                else if (c.name == SCS_TELEMETRY_CHANNEL_game_time) v.value_u32.value = 4000;
                else if (c.name == SCS_TELEMETRY_CHANNEL_local_scale) v.value_float.value = float(scale);
                else if (c.name == "trailer.0.connected") v.value_bool.value = 1;
                c.cb(c.name.c_str(), SCS_U32_NIL, pv, c.ctx);
            }
        }
        event(SCS_TELEMETRY_EVENT_frame_end);
    }
};

static scs_named_value_t attr_str(const char* name, const char* v) {
    scs_named_value_t a{};
    a.name = name; a.index = SCS_U32_NIL; a.value.type = SCS_VALUE_TYPE_string; a.value.value_string.value = v;
    return a;
}
static scs_named_value_t attr_u32(const char* name, scs_u32_t v) {
    scs_named_value_t a{};
    a.name = name; a.index = SCS_U32_NIL; a.value.type = SCS_VALUE_TYPE_u32; a.value.value_u32.value = v;
    return a;
}
static scs_named_value_t attr_s64(const char* name, scs_s64_t v) {
    scs_named_value_t a{};
    a.name = name; a.index = SCS_U32_NIL; a.value.type = SCS_VALUE_TYPE_s64; a.value.value_s64.value = v;
    return a;
}
static scs_named_value_t attr_float(const char* name, float v) {
    scs_named_value_t a{};
    a.name = name; a.index = SCS_U32_NIL; a.value.type = SCS_VALUE_TYPE_float; a.value.value_float.value = v;
    return a;
}

static bool copy_file(const std::string& from, const std::string& to) {
    std::FILE* in = pt::open_file_utf8(from, "rb");
    if (!in) return false;
    std::FILE* out = pt::open_file_utf8(to, "wb");
    if (!out) { std::fclose(in); return false; }
    char buf[65536];
    std::size_t n;
    while ((n = std::fread(buf, 1, sizeof buf, in)) > 0) std::fwrite(buf, 1, n, out);
    std::fclose(in);
    std::fclose(out);
    return true;
}

int main(int argc, char** argv) {
    REQUIRE(argc == 2);
    g_mock.main_thread = std::this_thread::get_id();
#ifdef _WIN32
    WSADATA wsa;
    WSAStartup(MAKEWORD(2, 2), &wsa);
#endif

    // Isolated copy of the plugin so its ini and journal live in a scratch dir.
    const std::string work = pt::join_path(pt::dir_of(pt::own_module_path()), "fake_game_run");
    pt::make_dirs(pt::join_path(work, "plasmatrack/journal"));
    for (const auto& f : pt::list_dir(pt::join_path(work, "plasmatrack/journal")))
        pt::remove_file(pt::join_path(pt::join_path(work, "plasmatrack/journal"), f));
    const std::string name = pt::dir_of(argv[1]) == "." ? argv[1] : std::string(argv[1]).substr(pt::dir_of(argv[1]).size() + 1);
    const std::string dll = pt::join_path(work, name);
    REQUIRE(copy_file(argv[1], dll));

    Client client;
    const std::uint16_t port = client.listen_any();
    {
        std::FILE* ini = pt::open_file_utf8(pt::join_path(work, "plasmatrack.ini"), "wb");
        REQUIRE(ini);
        std::fprintf(ini, "# written by pt_fake_game\nport = %u\nsample_rate_hz = 20\nreplay_buffer_s = 120\n", unsigned(port));
        std::fclose(ini);
    }

#ifdef _WIN32
    HMODULE lib = LoadLibraryA(dll.c_str());
    REQUIRE(lib);
    auto init = reinterpret_cast<init_fn>(reinterpret_cast<void*>(GetProcAddress(lib, "scs_telemetry_init")));
    auto shutdown = reinterpret_cast<shutdown_fn>(reinterpret_cast<void*>(GetProcAddress(lib, "scs_telemetry_shutdown")));
#else
    void* lib = dlopen(dll.c_str(), RTLD_NOW | RTLD_LOCAL);
    REQUIRE(lib);
    auto init = reinterpret_cast<init_fn>(dlsym(lib, "scs_telemetry_init"));
    auto shutdown = reinterpret_cast<shutdown_fn>(dlsym(lib, "scs_telemetry_shutdown"));
#endif
    REQUIRE(init && shutdown);

    scs_telemetry_init_params_v101_t params{};
    params.common.game_name = "Euro Truck Simulator 2";
    params.common.game_id = SCS_GAME_ID_EUT2;
    params.common.game_version = SCS_TELEMETRY_EUT2_GAME_VERSION_1_18;
    params.common.log = mock_log;
    params.register_for_event = mock_register_event;
    params.unregister_from_event = mock_unregister_event;
    params.register_for_channel = mock_register_channel;
    params.unregister_from_channel = mock_unregister_channel;

    // Version negotiation: only API 1.01 is accepted.
    CHECK(init(SCS_TELEMETRY_VERSION_1_00, &params) == SCS_RESULT_unsupported);
    {
        auto other = params;
        other.common.game_id = "some_other_game";
        CHECK(init(SCS_TELEMETRY_VERSION_1_01, &other) == SCS_RESULT_unsupported);
        g_mock.reset();
    }
    REQUIRE(init(SCS_TELEMETRY_VERSION_1_01, &params) == SCS_RESULT_ok);
    CHECK(g_mock.events.count(SCS_TELEMETRY_EVENT_gameplay) == 1);
    CHECK(g_mock.events.count(SCS_TELEMETRY_EVENT_configuration) == 1);
    CHECK(g_mock.channels.size() == 14);
    for (const auto& c : g_mock.channels)
        CHECK(c.flags == (SCS_TELEMETRY_CHANNEL_FLAG_each_frame | SCS_TELEMETRY_CHANNEL_FLAG_no_value));
    for (const auto& c : g_mock.channels)
        if (c.name == SCS_TELEMETRY_TRUCK_CHANNEL_world_placement) CHECK(c.type == SCS_VALUE_TYPE_dplacement);

    // ---- connect, handshake, integrity ----------------------------------------
    REQUIRE(client.accept_within(3000));
    REQUIRE(client.wait_for([](auto& v) {
        return has_kind(v, "hello") && has_kind(v, "snapshot") && has_kind(v, "init") &&
               has_kind(v, "integrity") && has_kind(v, "modules");
    }));
    const std::string sess1 = find_kind(client.received, "hello")->sess;
    CHECK(sess1.size() == 32);
    const Message* init_msg = find_kind(client.received, "init");
    CHECK(init_msg->json.find("\"game_id\":\"eut2\"") != std::string::npos);
    CHECK(init_msg->json.find("\"game_version\":{\"major\":1,\"minor\":18}") != std::string::npos);
    CHECK(init_msg->json.find("\"events_failed\":\"\"") != std::string::npos);
    const std::string expected_hash = pt::sha256_file(dll);
    CHECK(expected_hash.size() == 64);
    CHECK(find_kind(client.received, "integrity")->json.find("\"plugin_sha256\":\"" + expected_hash + "\"") != std::string::npos);
    CHECK(find_kind(client.received, "modules")->json.find(name) != std::string::npos);

    // ---- a short job ------------------------------------------------------------
    Game game;
    scs_named_value_t job_attrs[] = {
        attr_str(SCS_TELEMETRY_CONFIG_ATTRIBUTE_cargo_id, "apples"),
        attr_str(SCS_TELEMETRY_CONFIG_ATTRIBUTE_cargo, "\xC3\x84pfel"),  // UTF-8
        attr_float(SCS_TELEMETRY_CONFIG_ATTRIBUTE_cargo_mass, 20000.0f),
        attr_u32(SCS_TELEMETRY_CONFIG_ATTRIBUTE_planned_distance_km, 412),
        scs_named_value_t{},
    };
    scs_telemetry_configuration_t job_cfg{SCS_TELEMETRY_CONFIG_job, job_attrs};
    game.event(SCS_TELEMETRY_EVENT_configuration, &job_cfg);
    game.event(SCS_TELEMETRY_EVENT_started);

    scs_named_value_t delivered_attrs[] = {
        attr_s64(SCS_TELEMETRY_GAMEPLAY_EVENT_ATTRIBUTE_revenue, 12000),
        attr_float(SCS_TELEMETRY_GAMEPLAY_EVENT_ATTRIBUTE_distance_km, 1.25f),
        scs_named_value_t{},
    };
    scs_telemetry_gameplay_event_t delivered{SCS_TELEMETRY_GAMEPLAY_EVENT_job_delivered, delivered_attrs};
    double odo = 100.0;
    for (int i = 0; i < 120; ++i) {  // 2 s of simulation at 60 Hz
        odo += 20.0 * 0.016667 / 1000.0;
        if (i == 80) game.scale = 3.0;  // drive into a city
        game.frame(false, 20.0, odo, 300.0 - i * 0.01, 1000.0 + i, 0, i != 60, i == 100 ? &delivered : nullptr);
    }
    game.event(SCS_TELEMETRY_EVENT_paused);
    game.frame(true, 0, 0, 0, 0);

    REQUIRE(client.wait_for([&](auto& v) { return has_kind(v, "paused", sess1); }));
    {
        std::vector<std::uint64_t> seqs;
        int samples = 0;
        for (const auto& m : client.received) {
            if (m.sess == sess1 && m.seq) seqs.push_back(m.seq);
            if (m.kind == "sample") ++samples;
        }
        std::sort(seqs.begin(), seqs.end());
        CHECK(!seqs.empty() && seqs.front() == 1);
        bool contiguous = true;
        for (std::size_t i = 1; i < seqs.size(); ++i) contiguous &= seqs[i] == seqs[i - 1] + 1;
        CHECK(contiguous);
        // 20 Hz over 2 s = 40, plus samples forced by config/started/gameplay.
        CHECK(samples >= 40 && samples <= 46);
        if (samples < 40 || samples > 46) std::fprintf(stderr, "  samples=%d\n", samples);

        const Message* cfg = find_kind(client.received, "config", sess1);
        REQUIRE(cfg);
        CHECK(cfg->json.find("\"id\":\"job\"") != std::string::npos);
        CHECK(cfg->json.find("{\"name\":\"cargo.id\",\"index\":null,\"type\":\"string\",\"value\":\"apples\"}") != std::string::npos);
        CHECK(cfg->json.find("\"value\":\"\xC3\x84pfel\"") != std::string::npos);
        CHECK(cfg->json.find("{\"name\":\"planned_distance.km\",\"index\":null,\"type\":\"u32\",\"value\":412}") != std::string::npos);

        const Message* gp = find_kind(client.received, "gameplay", sess1);
        REQUIRE(gp);
        CHECK(gp->json.find("\"id\":\"job.delivered\"") != std::string::npos);
        CHECK(gp->json.find("{\"name\":\"revenue\",\"index\":null,\"type\":\"s64\",\"value\":12000}") != std::string::npos);
        CHECK(gp->json.find("{\"name\":\"distance.km\",\"index\":null,\"type\":\"float\",\"value\":1.25}") != std::string::npos);

        const Message* s = find_kind(client.received, "sample", sess1);
        REQUIRE(s);
        CHECK(s->json.find("\"truck.speed\":20") != std::string::npos);
        CHECK(s->json.find("\"truck.world.placement\":{\"position\":{\"x\":") != std::string::npos);
        CHECK(s->json.find("\"local.scale\":19") != std::string::npos);
        CHECK(s->json.find("\"trailer.0.connected\":true") != std::string::npos);

        // Scale changes are durable messages: the first value, then 19 -> 3.
        std::vector<const Message*> scales;
        for (const auto& m : client.received)
            if (m.sess == sess1 && m.kind == "scale") scales.push_back(&m);
        CHECK(scales.size() == 2);
        if (scales.size() == 2) {
            CHECK(scales[0]->json.find("\"local_scale\":19,\"previous\":null") != std::string::npos);
            CHECK(scales[1]->json.find("\"local_scale\":3,\"previous\":19") != std::string::npos);
            CHECK(scales[1]->json.find("\"truck.odometer\":") != std::string::npos);
            CHECK(scales[1]->json.find("\"truck.world.placement\":{\"position\"") != std::string::npos);
        }
    }

    // ---- ack part, disconnect, keep playing, reconnect: durable replay ----------
    const std::uint64_t acked = find_kind(client.received, "gameplay", sess1)->seq;
    client.send_ack(sess1, acked);
    std::this_thread::sleep_for(std::chrono::milliseconds(100));
    client.drop_connection();
    client.received.clear();

    scs_telemetry_configuration_t empty_job{SCS_TELEMETRY_CONFIG_job, job_attrs + 4};  // "no job"
    game.event(SCS_TELEMETRY_EVENT_configuration, &empty_job);
    game.event(SCS_TELEMETRY_EVENT_started);
    game.frame(false, 0, odo, 290, 2000, SCS_TELEMETRY_FRAME_START_FLAG_timer_restart);
    game.event(SCS_TELEMETRY_EVENT_paused);
    game.frame(true, 0, 0, 0, 0);

    REQUIRE(client.accept_within(8000));
    REQUIRE(client.wait_for([&](auto& v) { return has_kind(v, "timer_restart", sess1) && has_kind(v, "paused", sess1); }));
    {
        const Message* snap = find_kind(client.received, "snapshot");
        REQUIRE(snap);
        CHECK(snap->json.find("\"paused\":true") != std::string::npos);
        CHECK(snap->json.find("\"id\":\"job\",\"attrs\":[]") != std::string::npos);  // latest job config is empty
        bool replayed_acked = false;
        for (const auto& m : client.received)
            if (m.sess == sess1 && m.seq && m.seq <= acked && m.kind != "sample") replayed_acked = true;
        CHECK(!replayed_acked);
        CHECK(has_kind(client.received, "config", sess1));
    }

    // Heartbeat while paused.
    std::this_thread::sleep_for(std::chrono::milliseconds(1100));
    game.frame(true, 0, 0, 0, 0);
    CHECK(client.wait_for([&](auto& v) { return has_kind(v, "heartbeat", sess1); }, 2000));
    CHECK(find_kind(client.received, "heartbeat", sess1)->json.find("\"local_scale\":3") != std::string::npos);

    // Logs are flushed on the game thread only.
    game.frame(true, 0, 0, 0, 0);
    CHECK(std::any_of(g_mock.logs.begin(), g_mock.logs.end(),
                      [](const std::string& l) { return l.find("connected to client") != std::string::npos; }));

    // ---- sdk reload: shutdown (unacknowledged tail) and init again -----------------
    shutdown();
    CHECK(client.wait_for([&](auto& v) { return has_kind(v, "shutdown", sess1); }, 2000));
    client.drop_connection();
    client.received.clear();
    g_mock.reset();

    REQUIRE(init(SCS_TELEMETRY_VERSION_1_01, &params) == SCS_RESULT_ok);
    REQUIRE(client.accept_within(8000));
    REQUIRE(client.wait_for([&](auto& v) { return has_kind(v, "hello") && has_kind(v, "shutdown", sess1); }));
    const std::string sess2 = find_kind(client.received, "hello")->sess;
    CHECK(sess2 != sess1);
    CHECK(has_kind(client.received, "init", sess2));

    // Acknowledge both sessions; the old session's journal files disappear.
    std::uint64_t last1 = 0;
    for (const auto& m : client.received) if (m.sess == sess1) last1 = std::max(last1, m.seq);
    client.send_ack(sess1, last1);
    const std::string jdir = pt::join_path(work, "plasmatrack/journal");
    bool gone = false;
    for (int i = 0; i < 100 && !gone; ++i) {
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
        auto files = pt::list_dir(jdir);
        gone = std::none_of(files.begin(), files.end(), [&](const std::string& f) { return f.rfind(sess1, 0) == 0; });
    }
    CHECK(gone);

    shutdown();
    CHECK(g_mock.thread_violations == 0);
#ifdef _WIN32
    CHECK(FreeLibrary(lib) != 0);
#else
    CHECK(dlclose(lib) == 0);
#endif

    if (g_failures) {
        std::fprintf(stderr, "%d check(s) failed\n", g_failures);
        return 1;
    }
    std::printf("fake game harness passed (sessions %s, %s; %d log lines)\n", sess1.c_str(), sess2.c_str(),
                int(g_mock.logs.size()));
    return 0;
}
