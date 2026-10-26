# 27_decimal — NPDecimalNumber

**NPDecimalNumber** (`include/Foundation/NPDecimalNumber.hje/.jeti`) is the
`NSDecimalNumber` port: exact decimal arithmetic, so `0.1 + 0.2` is `0.3` and
not `0.30000000000000004`.

Representation follows `NSDecimal`: a 128-bit unsigned significand + a decimal
exponent + sign/status bits, i.e. `value = (-1)^sign * significand * 10^exp`,
with the same 38-significant-digit ceiling (2^128 ≈ 3.4e38).

- `+decimalNumberWithString:` — parses `[+-]?digits[.digits][(e|E)[+-]?digits]`;
  invalid input → `notANumber` (ObjC-aligned). Leading zeros do not consume the
  38-digit budget; digits beyond it are dropped and the RND bit is set.
- `+decimalNumberWithDouble:` — converts through the **shortest round-trip
  decimal spelling** (`0.1` → `"0.1"`), ObjC-aligned; the binary expansion
  never leaks in.
- `-decimalNumberByAdding:` / `Subtracting:` / `MultiplyingBy:` / `DividingBy:`
  — exact where the result fits 38 digits; division keeps up to 38 digits and
  rounds half-up (NSRoundPlain), the divisor's remainder deciding the round.
- `-compare:` / `-isEqualToNumber:` / `-decimalNumberByNegating`,
  `-doubleValue` / `-longLongValue` (truncation toward zero) / `-stringValue`.

Honest boundaries (see the header): overflow/underflow and division by zero do
**not** raise — ObjC throws `NSDecimalNumberOverflowException` /
`...DivideByZeroException`, while this port returns `notANumber` and sets the
status bits, so callers must check `-isNotANumber` (no silent truncation).
Operands whose exponents differ by more than the 38-digit window lose the
smaller term with the RND bit set (NSDecimal's loss-of-precision), rather than
going NaN.

Verified here:

| case | checked |
|------|---------|
| A1 | `0.1 + 0.2` is exactly `"0.3"` and `-isEqualToNumber:` `0.3` |
| A2 | `+decimalNumberWithDouble:` gives the shortest decimal spelling (`0.1`, `1.5`) |
| A3 | `1.1 × 3 = 3.3`, `10 ÷ 4 = 2.5` |
| A4 | `1 ÷ 3` → 38 threes (precision ceiling, half-up) |
| A5 | 38-digit addend: `12345678901234567890123456789012345678 + 1` |
| A6 | `-compare:` ordering (`0.3 > 0.25`, `-1 < 1`, `2 == 2`), `-decimalNumberByNegating` |
| A7 | invalid input / divide-by-zero → `notANumber`; NaN is equal to nothing |
| A8 | `-doubleValue` / `-longLongValue` (truncation toward zero) |

Output is deterministic — GOLDEN-MATCH line by line.

Note on receivers: the golden uses variable or nested-message receivers
(`[recv sel]`, `[[X alloc] init]`). A call receiver (`[f(x) sel]`) is parsed as
a subscript by jetic's documented `[`-disambiguation, so it is not used here.

While writing this class, four compiler defects surfaced and were fixed (each
with a probe under `probes/p2_decimal/`):

1. **Unsigned literals above `i64::MAX`** were emitted as the wrapped negative
   `i64` with the `U`/`ULL` suffix suppressed — the C compiler then evaluated
   them as *signed* (`(int)(18446744073709551615ull % 10)` came out `-7`).
   Now re-interpreted modulo 2^64 with the suffix kept.
2. **Float literals** were emitted with an `f` suffix (`double d = 0.1f;`),
   silently dropping the low bits (`0.10000000149011612` instead of `0.1`).
   Now emitted as C doubles, matching the checker's `double` typing.
3. **Struct fields in a multi-declarator declaration** (`uint64_t hi, lo;`)
   emitted the second field as `int lo;` — the parser left its type empty and
   codegen fell back to `int`. The head declarator's base type is now carried.
4. **Array suffixes on later declarators** (`char a[4], b[4];`) were emitted
   before the name (`char[4] b;`, invalid C); the suffix now follows the name.
