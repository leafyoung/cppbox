#include <atomic>
#include <iostream>
#include <thread>
#include <vector>

int main() {
    std::atomic<int> counter{0};
    std::vector<std::thread> threads;

    for (int i = 0; i < 4; ++i) {
        threads.emplace_back([&] {
            for (int j = 0; j < 1'000'000; ++j) {
                counter.fetch_add(1);
            }
        });
    }

    for (auto& t : threads) t.join();

    std::cout << counter << '\n';
    return 0;
}
