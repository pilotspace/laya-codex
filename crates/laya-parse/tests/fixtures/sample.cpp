#include <string>
#include <vector>

namespace geo {

// A 2D point.
class Point {
public:
    Point(double x, double y) : x_(x), y_(y) {}

    double norm() const {
        return x_ * x_ + y_ * y_;
    }

    Point scaled(double k) const {
        return Point(x_ * k, y_ * k);
    }

private:
    double x_;
    double y_;
};

double Distance(const Point& a, const Point& b) {
    return a.norm() - b.norm();
}

}  // namespace geo

int main() {
    geo::Point p(1, 2);
    return static_cast<int>(p.norm());
}
