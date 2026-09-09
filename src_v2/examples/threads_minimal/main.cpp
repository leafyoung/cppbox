#include <cstdio>
#include <thread>

int main() {
    printf("before spawn\n");
    fflush(stdout);
    std::thread t([]{
        printf("in thread\n");
    });
    printf("spawned, joining\n");
    fflush(stdout);
    t.join();
    printf("joined\n");
    return 0;
}
