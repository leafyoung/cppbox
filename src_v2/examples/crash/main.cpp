#include <cstdio>
#include <cstdlib>

int main() {
    printf("about to crash\n");
    fflush(stdout);
    // Deliberately abort - this is what an assert()/unhandled-invariant
    // failure looks like. We want the harness to see a nonzero exit /
    // trap, not a hang or a silently-swallowed failure.
    std::abort();
}
