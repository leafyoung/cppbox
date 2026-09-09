#include <algorithm>
#include <iostream>
#include <vector>

int main() {
    std::vector<int> x{3, 1, 2};
    std::sort(x.begin(), x.end());
    for (auto v : x) std::cout << v << '\n';
    return 0;
}
