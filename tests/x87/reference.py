"""Independent reference for armor's x87 transcendentals (ADR-0030).

Python's decimal module at 220 digits (ln, exp and sqrt from the module;
pi by Machin's formula, atan, sin and cos by series written here), then
the exact value is rounded to the 64-bit significand in the case's
rounding mode, with fractions. Each case is "op mode a b result flags
rounded_up" in hex, as the ignored test `write_cases` of
crates/armor-cpu/src/f80/transcendental.rs writes them; the result and
the round-up indicator (C1) must match. docs/testing.md tells how to run
it. Usage: python3 -I tests/x87/reference.py CASES"""
import sys
from decimal import Decimal as D, getcontext
from fractions import Fraction as F

getcontext().prec = 220
BIAS = 16383

def f80(h):
    sign = h[0] == '-'
    exp = int(h[1:5], 16)
    mant = int(h[5:], 16)
    if exp == 0:
        v = F(mant, 1) * F(2) ** (1 - BIAS - 63)
    else:
        v = F(mant, 1) * F(2) ** (exp - BIAS - 63)
    return -v if sign else v

def dec(fr):
    return D(fr.numerator) / D(fr.denominator)

def frac(d):
    return F(d)

PI = None
def pi():
    global PI
    if PI is None:
        # Machin: pi = 16 atan(1/5) - 4 atan(1/239).
        def at(inv):
            x = D(1) / inv; x2 = x * x; s = x; t = x; k = 1
            while True:
                t = -t * x2; term = t / (2 * k + 1)
                if abs(term) < D(10) ** -(getcontext().prec + 5): break
                s += term; k += 1
            return s
        PI = 16 * at(D(5)) - 4 * at(D(239))
    return PI

LN2 = D(2).ln()

def atan(z):
    # z >= 0
    if z > 1:
        return pi() / 2 - atan(1 / z)
    # two halvings: atan(z) = 2 atan(z / (1 + sqrt(1 + z^2)))
    z = z / (1 + (1 + z * z).sqrt())
    z = z / (1 + (1 + z * z).sqrt())
    x2 = z * z; s = z; t = z; k = 1
    while True:
        t = -t * x2; term = t / (2 * k + 1)
        if abs(term) < abs(s) * D(10) ** -(getcontext().prec + 5): break
        s += term; k += 1
    return 4 * s

def sincos(x):
    p2 = 2 * pi()
    k = (x / p2).to_integral_value()
    r = x - k * p2
    r2 = r * r
    s = r; t = r; n = 1
    while True:
        t = -t * r2 / ((2 * n) * (2 * n + 1))
        if t == 0 or abs(t) < abs(s) * D(10) ** -(getcontext().prec + 5): break
        s += t; n += 1
    c = D(1); t = D(1); n = 1
    while True:
        t = -t * r2 / ((2 * n - 1) * (2 * n))
        if t == 0 or abs(t) < D(10) ** -(getcontext().prec + 5): break
        c += t; n += 1
    return s, c

def expm1(t):
    if abs(t) > D('0.001'):
        return t.exp() - 1
    s = t; term = t; k = 2
    while True:
        term = term * t / k
        if abs(term) < abs(s) * D(10) ** -(getcontext().prec + 5): break
        s += term; k += 1
    return s

def log2(x):
    return x.ln() / LN2

def log1p(x):
    if abs(x) > D('0.001'):
        return (1 + x).ln()
    s = x; term = x; k = 2
    while True:
        term = -term * x
        t = term / k
        if abs(t) < abs(s) * D(10) ** -(getcontext().prec + 5): break
        s += t; k += 1
    return s

def reference(op, a, b):
    if op == 'f2xm1': return expm1(dec(a) * LN2)
    if op == 'fyl2x': return dec(a) * log2(dec(b))
    if op == 'fyl2xp1': return dec(a) * log1p(dec(b)) / LN2
    if op == 'fpatan':
        y, x = dec(a), dec(b)
        t = atan(abs(y) / abs(x))
        if x < 0: t = pi() - t
        return -t if y < 0 else t
    s, c = sincos(dec(a))
    return {'fsin': s, 'fcos': c, 'fptan': s / c}[op]

def round64(v, rc):
    """(rounded value, rounded up, exact), for nonzero v, unbounded exponent."""
    neg = v < 0
    m = abs(v)
    e = m.numerator.bit_length() - m.denominator.bit_length()
    while F(2) ** e > m: e -= 1
    while F(2) ** (e + 1) <= m: e += 1
    scaled = m / F(2) ** (e - 63)
    lo = scaled.numerator // scaled.denominator
    rest = scaled - lo
    if rest == 0:
        return (-lo if neg else lo) * F(2) ** (e - 63), False
    if rc == 'Nearest':
        up = rest > F(1, 2) or (rest == F(1, 2) and lo % 2 == 1)
    elif rc == 'Zero':
        up = False
    elif rc == 'Up':
        up = not neg
    else:
        up = neg
    q = lo + 1 if up else lo
    r = q * F(2) ** (e - 63)
    return (-r if neg else r), up

bad = 0; total = 0; undecided = 0
for line in open(sys.argv[1]):
    op, rc, ha, hb, hr, flags, up = line.split()
    a, b, got = f80(ha), f80(hb), f80(hr)
    ref = reference(op, a, b)
    total += 1
    # The reference errs by ~1e-215 relative: check both ends round alike.
    eps = abs(ref) * D(10) ** -210
    r1 = round64(frac(ref - eps), rc); r2 = round64(frac(ref + eps), rc)
    if r1 != r2:
        undecided += 1
        continue
    expect, expect_up = r1
    if expect != got or bool(int(up)) != expect_up:
        bad += 1
        if bad <= 15:
            print('MISMATCH', line.strip(), 'expected', float(expect), 'up', expect_up, 'got', float(got))
print(f'{total} cases, {bad} mismatches, {undecided} undecided')
