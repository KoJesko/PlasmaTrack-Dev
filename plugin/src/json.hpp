// Minimal streaming JSON writer.
//
// Numbers are formatted with std::to_chars, which is locale-independent and
// round-trips exactly (shortest representation). printf-style formatting is not
// used anywhere in the plugin because the game process may change the C locale.
#pragma once

#include <charconv>
#include <cmath>
#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

namespace pt {

class JsonWriter {
public:
    JsonWriter() { out_.reserve(256); }

    JsonWriter& begin_object() { value_prefix(); out_ += '{'; first_.push_back(true); return *this; }
    JsonWriter& end_object() { out_ += '}'; first_.pop_back(); return *this; }
    JsonWriter& begin_array() { value_prefix(); out_ += '['; first_.push_back(true); return *this; }
    JsonWriter& end_array() { out_ += ']'; first_.pop_back(); return *this; }

    JsonWriter& key(std::string_view k) {
        comma();
        write_string(k);
        out_ += ':';
        after_key_ = true;
        return *this;
    }

    JsonWriter& str(std::string_view s) { value_prefix(); write_string(s); return *this; }
    JsonWriter& null() { value_prefix(); out_ += "null"; return *this; }
    JsonWriter& boolean(bool b) { value_prefix(); out_ += b ? "true" : "false"; return *this; }
    JsonWriter& u64(std::uint64_t v) { value_prefix(); append_int(v); return *this; }
    JsonWriter& i64(std::int64_t v) { value_prefix(); append_int(v); return *this; }

    // Non-finite values are not representable in JSON and are written as null.
    JsonWriter& f32(float v) { value_prefix(); append_float(v); return *this; }
    JsonWriter& f64(double v) { value_prefix(); append_float(v); return *this; }

    // Splices pre-rendered JSON (must itself be a valid JSON value).
    JsonWriter& raw(std::string_view json) { value_prefix(); out_ += json; return *this; }

    const std::string& str_ref() const { return out_; }
    std::string take() { return std::move(out_); }

private:
    void comma() {
        if (!first_.empty()) {
            if (!first_.back()) out_ += ',';
            first_.back() = false;
        }
    }

    void value_prefix() {
        if (after_key_) { after_key_ = false; return; }
        comma();
    }

    template <typename T>
    void append_int(T v) {
        char buf[32];
        auto r = std::to_chars(buf, buf + sizeof buf, v);
        out_.append(buf, r.ptr);
    }

    template <typename T>
    void append_float(T v) {
        if (!std::isfinite(v)) { out_ += "null"; return; }
        char buf[64];
        auto r = std::to_chars(buf, buf + sizeof buf, v);
        out_.append(buf, r.ptr);
    }

    void write_string(std::string_view s) {
        static const char hex[] = "0123456789abcdef";
        out_ += '"';
        for (unsigned char c : s) {
            switch (c) {
                case '"': out_ += "\\\""; break;
                case '\\': out_ += "\\\\"; break;
                case '\n': out_ += "\\n"; break;
                case '\r': out_ += "\\r"; break;
                case '\t': out_ += "\\t"; break;
                default:
                    if (c < 0x20) {
                        out_ += "\\u00";
                        out_ += hex[c >> 4];
                        out_ += hex[c & 0xf];
                    } else {
                        out_ += static_cast<char>(c);  // UTF-8 passes through unchanged
                    }
            }
        }
        out_ += '"';
    }

    std::string out_;
    std::vector<bool> first_;
    bool after_key_ = false;
};

}  // namespace pt
