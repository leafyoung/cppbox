#include <iostream>
#include <string>

int main() {
    std::string line;
    int total = 0;
    while (std::getline(std::cin, line)) {
        if (line.empty()) continue;
        total += std::stoi(line);
    }
    std::cout << "sum=" << total << '\n';
    return 0;
}
