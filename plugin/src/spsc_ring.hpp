// Bounded lock-free single-producer / single-consumer ring.
//
// The producer never blocks: when the ring is full, try_push fails and the
// caller drops the item (and counts the drop). Used game thread -> sender thread
// for telemetry, and sender thread -> game thread for log lines.
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdlib>
#include <memory>
#include <utility>

namespace pt {

template <typename T>
class SpscRing {
public:
    explicit SpscRing(std::size_t capacity_pow2)
        : mask_(capacity_pow2 - 1), slots_(new T[capacity_pow2]) {
        // Capacity must be a power of two so indices wrap with a mask.
        if (capacity_pow2 == 0 || (capacity_pow2 & mask_) != 0) std::abort();
    }

    SpscRing(const SpscRing&) = delete;
    SpscRing& operator=(const SpscRing&) = delete;

    // Producer side only.
    bool try_push(T&& item) {
        const std::size_t head = head_.load(std::memory_order_relaxed);
        const std::size_t tail = tail_.load(std::memory_order_acquire);
        if (head - tail > mask_) return false;  // full
        slots_[head & mask_] = std::move(item);
        head_.store(head + 1, std::memory_order_release);
        return true;
    }

    // Consumer side only.
    bool try_pop(T& out) {
        const std::size_t tail = tail_.load(std::memory_order_relaxed);
        const std::size_t head = head_.load(std::memory_order_acquire);
        if (tail == head) return false;  // empty
        out = std::move(slots_[tail & mask_]);
        tail_.store(tail + 1, std::memory_order_release);
        return true;
    }

    std::size_t capacity() const { return mask_ + 1; }

private:
    const std::size_t mask_;
    std::unique_ptr<T[]> slots_;
    alignas(64) std::atomic<std::size_t> head_{0};
    alignas(64) std::atomic<std::size_t> tail_{0};
};

}  // namespace pt
