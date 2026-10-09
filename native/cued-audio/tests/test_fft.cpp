// Port of FftTest.`pure tone peaks in the right bin` plus a Hann window check.
#include <algorithm>
#include <cmath>
#include <vector>

#include "fft.h"
#include "test_util.h"

int main() {
    using namespace cued;
    {
        const int n = 1024;
        Fft fft(n);
        const float sr = 44100.0f;
        const float freq = sr / n * 100; // exactly bin 100
        std::vector<float> input(n), ones(n, 1.0f), re(n), im(n), out(n / 2);
        for (int i = 0; i < n; ++i) input[i] = static_cast<float>(std::sin(2.0 * M_PI * freq * i / sr));
        fft.magnitudes(input.data(), n, ones.data(), re.data(), im.data(), out.data());
        const int peak = static_cast<int>(std::max_element(out.begin(), out.end()) - out.begin());
        CHECK_MSG(peak == 100, "peak bin %d", peak);
        CHECK(out[100] > 10 * out[50]);
        // A full-scale sine of N samples has magnitude N/2 at its bin.
        CHECK_NEAR(out[100], n / 2.0f, 0.5f);
    }
    {
        // Hann window: zero at both ends, one in the middle, symmetric.
        const int n = 512;
        auto w = Fft::hann(n);
        CHECK_NEAR(w[0], 0.0f, 1e-6f);
        CHECK_NEAR(w[n - 1], 0.0f, 1e-6f);
        CHECK_NEAR(w[(n - 1) / 2], 1.0f, 1e-4f);
        for (int i = 0; i < n; ++i) CHECK_NEAR(w[i], w[n - 1 - i], 1e-6f);
        // Same formula as the phone: 0.5 * (1 - cos(2*pi*i/(size-1))).
        CHECK_NEAR(w[10], static_cast<float>(0.5 * (1.0 - std::cos(2.0 * M_PI * 10 / (n - 1)))), 1e-7f);
    }
    {
        // Invalid sizes are rejected.
        bool threw = false;
        try { Fft bad(1000); } catch (const std::invalid_argument&) { threw = true; }
        CHECK(threw);
        // Inverse-ish sanity: Parseval for a random signal.
        const int n = 256;
        test::JavaRandom rnd(3);
        std::vector<float> re(n), im(n, 0.0f);
        double timeEnergy = 0.0;
        for (int i = 0; i < n; ++i) { re[i] = rnd.nextFloat() * 2.0f - 1.0f; timeEnergy += re[i] * re[i]; }
        Fft(n).transform(re.data(), im.data());
        double freqEnergy = 0.0;
        for (int i = 0; i < n; ++i) freqEnergy += re[i] * re[i] + im[i] * im[i];
        CHECK_NEAR(freqEnergy / n, timeEnergy, 1e-3);
    }
    return test::finish("fft");
}
