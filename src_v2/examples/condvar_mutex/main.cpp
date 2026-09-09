#include <condition_variable>
#include <iostream>
#include <mutex>
#include <queue>
#include <thread>

int main() {
    std::mutex m;
    std::condition_variable cv;
    std::queue<int> q;
    bool done = false;

    std::thread producer([&] {
        for (int i = 1; i <= 5; ++i) {
            {
                std::lock_guard<std::mutex> lk(m);
                q.push(i);
            }
            cv.notify_one();
        }
        {
            std::lock_guard<std::mutex> lk(m);
            done = true;
        }
        cv.notify_one();
    });

    int sum = 0;
    std::thread consumer([&] {
        while (true) {
            std::unique_lock<std::mutex> lk(m);
            cv.wait(lk, [&] { return !q.empty() || done; });
            while (!q.empty()) {
                sum += q.front();
                q.pop();
            }
            if (done && q.empty()) break;
        }
    });

    producer.join();
    consumer.join();

    std::cout << "sum=" << sum << '\n';
    return 0;
}
