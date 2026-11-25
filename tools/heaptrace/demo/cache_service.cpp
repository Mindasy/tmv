// g++ -std=c++17 -O0 -g -fno-omit-frame-pointer -Wl,--build-id -pthread cache_service.cpp -o cache_service
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <memory>
#include <thread>
#include <vector>

namespace service {
using namespace std::chrono_literals;
constexpr std::size_t MiB = 1024 * 1024;
using Buffer = std::unique_ptr<unsigned char[]>;

struct Thumbnail { std::size_t bytes; };
struct Original { std::size_t bytes; };

class ImageCache {
    std::vector<Buffer> pages_;
public:
    void insert(Thumbnail t) {
        // Distinct allocation PCs in one function; both belong to this overload.
        Buffer pixels(new unsigned char[t.bytes]());
        Buffer preview(new unsigned char[t.bytes / 2]());
        pages_.push_back(std::move(pixels));
        pages_.push_back(std::move(preview));
    }
    void insert(Original o) {
        pages_.emplace_back(new unsigned char[o.bytes]());
    }
};

struct Posting { std::uint64_t document, position; };
template<class T>
struct Index {
    std::unique_ptr<T[]> entries;
    void rebuild(std::size_t count) { entries.reset(new T[count]()); }
};

struct DecodeJob {
    Buffer scratch;
    void decode() { scratch.reset(new unsigned char[32 * MiB]()); }
};

void serve() {
    ImageCache cache;
    cache.insert(Thumbnail{8 * MiB});
    cache.insert(Original{24 * MiB});
    std::puts("warm: 36 MiB image cache");
    std::this_thread::sleep_for(6s);

    Index<Posting> index;
    DecodeJob job;
    std::thread worker([&] {
        index.rebuild(16 * MiB / sizeof(Posting));
        job.decode();
    });
    worker.join();
    std::puts("peak: cache + index + decode scratch");
    std::this_thread::sleep_for(6s);

    job.scratch.reset();
    std::puts("steady: cache + index");
    std::this_thread::sleep_for(6s);
}
} // namespace service

int main() { service::serve(); }
