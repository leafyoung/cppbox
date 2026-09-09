#include <filesystem>
#include <fstream>
#include <iostream>

namespace fs = std::filesystem;

int main() {
    const fs::path p = "out.txt";

    {
        std::ofstream out(p);
        out << "hello from wasm\n";
    }

    std::cout << "exists: " << fs::exists(p) << '\n';
    std::cout << "size: " << fs::file_size(p) << '\n';

    std::ifstream in(p);
    std::string line;
    std::getline(in, line);
    std::cout << "content: " << line << '\n';

    return 0;
}
