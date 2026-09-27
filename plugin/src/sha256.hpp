// SHA-256 (FIPS 180-4), used to report the plugin's own file hash (rule 0).
#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

namespace pt {

class Sha256 {
public:
    Sha256();
    void update(const void* data, std::size_t len);
    // Returns the digest as 64 lowercase hex characters. Call once.
    std::string hex_digest();

private:
    void block(const std::uint8_t* p);
    std::uint32_t h_[8];
    std::uint8_t buf_[64];
    std::size_t buf_len_ = 0;
    std::uint64_t total_len_ = 0;
};

// Hashes a file; returns an empty string if it cannot be read.
std::string sha256_file(const std::string& utf8_path);

}  // namespace pt
