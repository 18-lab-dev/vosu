// Reproduce reference values: c++ -std=c++17 aggregation-reference.cpp -o /tmp/vosu-rating-reference && /tmp/vosu-rating-reference
// Ported verbatim in behavior from Etterna 0.75.1, commit
// 2d706147cc4bcf95a8ceb24dc4f4129be207f803, MinaCalcHelpers.h.
// Copyright (c) 2016-2023 Etterna <etternadev@gmail.com>; MIT (../LICENSE-ETTERNA).
#include <algorithm>
#include <cmath>
#include <cstdio>
#include <vector>
static float aggregate(const std::vector<float>& v) {
    float rating = 0.0F, resolution = 10.24F;
    for (int i = 0; i < 11; i++) {
        double sum;
        do {
            rating += resolution;
            sum = 0.0;
            for (const auto& vv : v) {
                sum += std::max(0.0, 2.F / erfc(0.1L * (vv - rating)) - 2);
            }
        } while (pow(2, rating * 0.1) < sum);
        rating -= resolution;
        resolution /= 2.F;
    }
    rating += resolution * 2.F;
    return rating * 1.05F;
}
int main() {
    for (const auto& v : {std::vector<float>{20}, {20,20}, {10,20,30}})
        printf("%.9f\n", aggregate(v));
}
