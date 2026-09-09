#include <cstdio>

int main() {
    printf("looping forever\n");
    fflush(stdout);
    volatile int x = 0;
    while (true) {
        x = x + 1;
    }
    return 0;
}
