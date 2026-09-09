#include <iostream>
#include <stdexcept>
#include <vector>

int riskyDivide(int a, int b) {
    if (b == 0) throw std::invalid_argument("division by zero");
    return a / b;
}

int main() {
    try {
        std::cout << riskyDivide(10, 2) << '\n';
        std::cout << riskyDivide(10, 0) << '\n';
    } catch (const std::invalid_argument& e) {
        std::cout << "caught: " << e.what() << '\n';
    }

    try {
        std::vector<int> v{1, 2, 3};
        std::cout << v.at(10) << '\n';
    } catch (const std::out_of_range& e) {
        std::cout << "caught: " << e.what() << '\n';
    }

    std::cout << "done\n";
    return 0;
}
