#include "MinaCalc/MinaCalc.h"
#include <algorithm>
#include <cmath>
#include <cstddef>

// Do not expose a C++ class, allocation, exception, or STL type across the ABI.
extern "C" int vosu_minacalc(const NoteInfo* rows, size_t count,
                             unsigned keys, float rate, float goal,
                             bool ssr, float* out) noexcept {
    try {
        if (count <= 1) {
            std::fill(out, out + 8, 0.F);
            return 0;
        }
        Calc calc;
        std::vector<NoteInfo> notes(rows, rows + count);
        calc.keycount = keys;
        calc.ssr = ssr;
        auto result = ssr ? MinaSDCalc(notes, rate, goal, keys, &calc)
                          : calc.CalcMain(notes, rate, 0.93F);
        if (result.size() != 8) return 1;
        for (size_t i = 0; i < 8; ++i) {
            if (!std::isfinite(result[i])) return 2;
            out[i] = result[i];
        }
        return 0;
    } catch (...) {
        return 3;
    }
}
