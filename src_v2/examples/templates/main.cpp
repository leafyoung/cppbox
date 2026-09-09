#include <iostream>
#include <string>

template <typename T>
T maxOf(T a, T b) {
    return a > b ? a : b;
}

template <typename T>
class Box {
public:
    explicit Box(T v) : value_(v) {}
    T get() const { return value_; }

private:
    T value_;
};

int main() {
    std::cout << maxOf(3, 7) << '\n';
    std::cout << maxOf(2.5, 1.5) << '\n';
    std::cout << maxOf(std::string("abc"), std::string("abd")) << '\n';
    Box<int> b(42);
    std::cout << b.get() << '\n';
    return 0;
}
