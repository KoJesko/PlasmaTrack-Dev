// Unit tests for the plugin's building blocks. No framework: CHECK counts failures.
#include <atomic>
#include <cstdio>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

#include "journal.hpp"
#include "json.hpp"
#include "platform.hpp"
#include "settings.hpp"
#include "sha256.hpp"
#include "spsc_ring.hpp"
#include "value_json.hpp"

static int g_failures = 0;
#define CHECK(cond)                                                          \
    do {                                                                     \
        if (!(cond)) {                                                       \
            std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                    \
        }                                                                    \
    } while (0)

using namespace pt;

static std::string sha(const std::string& s) {
    Sha256 h;
    h.update(s.data(), s.size());
    return h.hex_digest();
}

static void test_sha256() {
    // FIPS 180-4 / NIST CAVS examples.
    CHECK(sha("") == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    CHECK(sha("abc") == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    CHECK(sha("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq") ==
          "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
    Sha256 h;
    std::string chunk(1000, 'a');
    for (int i = 0; i < 1000; ++i) h.update(chunk.data(), chunk.size());
    CHECK(h.hex_digest() == "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
}

static void test_json() {
    JsonWriter w;
    w.begin_object();
    w.key("s").str("a\"b\\c\n\x01");
    w.key("f").f32(46.939f);
    w.key("d").f64(0.1);
    w.key("nan").f64(std::nan(""));
    w.key("neg").i64(-5);
    w.key("big").u64(18446744073709551615ull);
    w.key("arr").begin_array();
    w.boolean(true).null().str("x");
    w.end_array();
    w.key("obj").begin_object().key("k").u64(1).end_object();
    w.key("utf8").str("K\xC3\xB6ln");
    w.end_object();
    const std::string expect =
        "{\"s\":\"a\\\"b\\\\c\\n\\u0001\",\"f\":46.939,\"d\":0.1,\"nan\":null,\"neg\":-5,"
        "\"big\":18446744073709551615,\"arr\":[true,null,\"x\"],\"obj\":{\"k\":1},\"utf8\":\"K\xC3\xB6ln\"}";
    CHECK(w.str_ref() == expect);
    if (w.str_ref() != expect) std::fprintf(stderr, "  got: %s\n", w.str_ref().c_str());
}

static void test_value_json() {
    scs_named_value_t attrs[4] = {};
    attrs[0].name = "cargo.id";
    attrs[0].index = SCS_U32_NIL;
    attrs[0].value.type = SCS_VALUE_TYPE_string;
    attrs[0].value.value_string.value = "apples";
    attrs[1].name = "income";
    attrs[1].index = SCS_U32_NIL;
    attrs[1].value.type = SCS_VALUE_TYPE_u64;
    attrs[1].value.value_u64.value = 12345;
    attrs[2].name = "wheel.position";
    attrs[2].index = 3;
    attrs[2].value.type = SCS_VALUE_TYPE_fvector;
    attrs[2].value.value_fvector = {1.5f, -2.0f, 0.25f};
    attrs[3].name = nullptr;  // terminator
    JsonWriter w;
    write_attributes(w, attrs);
    const std::string expect =
        "[{\"name\":\"cargo.id\",\"index\":null,\"type\":\"string\",\"value\":\"apples\"},"
        "{\"name\":\"income\",\"index\":null,\"type\":\"u64\",\"value\":12345},"
        "{\"name\":\"wheel.position\",\"index\":3,\"type\":\"fvector\",\"value\":{\"x\":1.5,\"y\":-2,\"z\":0.25}}]";
    CHECK(w.str_ref() == expect);
    if (w.str_ref() != expect) std::fprintf(stderr, "  got: %s\n", w.str_ref().c_str());

    scs_value_t p{};
    p.type = SCS_VALUE_TYPE_dplacement;
    p.value_dplacement.position = {-30120.25, 50.5, 5847.125};
    p.value_dplacement.orientation = {0.25f, 0.0f, -0.5f};
    JsonWriter w2;
    write_value(w2, p);
    CHECK(w2.str_ref() ==
          "{\"position\":{\"x\":-30120.25,\"y\":50.5,\"z\":5847.125},\"orientation\":{\"heading\":0.25,\"pitch\":0,\"roll\":-0.5}}");
}

static void test_ring() {
    SpscRing<int> r(4);
    int out = 0;
    CHECK(!r.try_pop(out));
    for (int i = 0; i < 4; ++i) CHECK(r.try_push(int(i)));
    CHECK(!r.try_push(99));  // full: producer drops, never blocks
    for (int i = 0; i < 4; ++i) { CHECK(r.try_pop(out)); CHECK(out == i); }
    CHECK(!r.try_pop(out));

    // Concurrent: every pushed item is received exactly once, in order.
    SpscRing<std::uint64_t> big(1024);
    constexpr std::uint64_t N = 2'000'000;
    std::atomic<std::uint64_t> dropped{0};
    std::thread producer([&] {
        for (std::uint64_t i = 1; i <= N; ++i) {
            std::uint64_t v = i;
            if (!big.try_push(std::move(v))) dropped.fetch_add(1);
        }
    });
    std::uint64_t last = 0, received = 0, v = 0;
    bool ordered = true;
    for (;;) {
        if (big.try_pop(v)) {
            if (v <= last) ordered = false;
            last = v;
            ++received;
            continue;
        }
        if (received + dropped.load() >= N) break;
    }
    producer.join();
    while (big.try_pop(v)) { if (v <= last) ordered = false; last = v; ++received; }
    CHECK(ordered);
    CHECK(received + dropped.load() == N);
}

static void test_settings() {
    Settings d = parse_settings("");
    CHECK(d.port == 30455 && d.sample_rate_hz == 20.0 && !d.capture && d.replay_buffer_s == 120.0);
    Settings s = parse_settings(
        "# comment\r\nport = 40001\r\nsample_rate_hz=12.5 ; trailing\ncapture = 1\nreplay_buffer_s = 30\n"
        "bogus = 7\njournal_max_mib = 8\n");
    CHECK(s.port == 40001);
    CHECK(s.sample_rate_hz == 12.5);
    CHECK(s.capture);
    CHECK(s.replay_buffer_s == 30.0);
    CHECK(s.journal_max_mib == 8);
    Settings bad = parse_settings("port = 70000\nsample_rate_hz = abc\nsample_rate_hz = -1\n");
    CHECK(bad.port == 30455 && bad.sample_rate_hz == 20.0);
}

static std::string msg(const char* sess, std::uint64_t seq) {
    return std::string("{\"v\":1,\"kind\":\"config\",\"sess\":\"") + sess + "\",\"seq\":" + std::to_string(seq) + "}";
}

static void test_journal() {
    const std::string dir = join_path(dir_of(own_module_path()), "pt_journal_test");
    for (const auto& f : list_dir(dir)) remove_file(join_path(dir, f));

    {
        Journal j(dir, "sessA", 1 << 20);
        CHECK(j.pending().empty());
        CHECK(j.append(1, msg("sessA", 1)));
        CHECK(j.append(2, msg("sessA", 2)));
        CHECK(j.append(5, msg("sessA", 5)));
        j.ack("sessA", 2);
        CHECK(j.pending().at("sessA").size() == 1);
    }
    {
        // Next game session: sessA's unacknowledged message survives on disk.
        Journal j(dir, "sessB", 1 << 20);
        CHECK(j.pending().count("sessA") == 1);
        CHECK(j.pending().at("sessA").size() == 1);
        CHECK(j.pending().at("sessA").front().seq == 5);
        CHECK(Journal::seq_of(j.pending().at("sessA").front().json) == 5);
        j.ack("sessA", 5);
        CHECK(j.pending().count("sessA") == 0);
    }
    {
        Journal j(dir, "sessC", 1 << 20);
        CHECK(j.pending().count("sessA") == 0);  // files were deleted after the full ack
        CHECK(j.pending().count("sessB") == 0);  // sessB never wrote anything
    }
    {
        // Size cap: over the cap, messages stay in memory but are not written.
        Journal j(dir, "sessD", 100);
        CHECK(j.append(1, std::string(60, 'x')));
        CHECK(!j.append(2, std::string(60, 'y')));
        CHECK(j.dropped() == 1);
    }
    for (const auto& f : list_dir(dir)) remove_file(join_path(dir, f));
}

int main() {
    test_sha256();
    test_json();
    test_value_json();
    test_ring();
    test_settings();
    test_journal();
    if (g_failures) {
        std::fprintf(stderr, "%d check(s) failed\n", g_failures);
        return 1;
    }
    std::printf("all unit tests passed\n");
    return 0;
}
