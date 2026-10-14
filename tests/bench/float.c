/*
 * Benchmark (cargo xtask bench): scalar double-precision floating point,
 * a Mandelbrot set and a numerical integration with sqrt and division.
 * Built with -fno-tree-vectorize: scalar SSE only (armor does not run
 * packed floating point yet).
 */
#include <math.h>
#include <stdio.h>

int main(void) {
    long inside = 0;
    for (int py = 0; py < 300; py++) {
        for (int px = 0; px < 400; px++) {
            double cr = -2.0 + px * (3.0 / 400), ci = -1.2 + py * (2.4 / 300);
            double zr = 0, zi = 0;
            int k = 0;
            while (k < 200 && zr * zr + zi * zi < 4.0) {
                double t = zr * zr - zi * zi + cr;
                zi = 2 * zr * zi + ci;
                zr = t;
                k++;
            }
            inside += k == 200;
        }
    }
    double integral = 0;
    const int steps = 4000000;
    for (int i = 0; i < steps; i++) {
        double x = (i + 0.5) / steps;
        integral += sqrt(1.0 - x * x) / (1.0 + x);
    }
    printf("%ld %.12f\n", inside, integral);
    return 0;
}
