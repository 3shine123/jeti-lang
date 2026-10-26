# 28_date_format — NPDateFormatter

**NPDateFormatter** (`include/Foundation/NPDateFormatter.hje/.jeti`) is the
`NSDateFormatter` port, v1 scope: ISO 8601, date-only and time-only shapes, and
a token-subset custom pattern.

Epoch seconds ↔ civil date conversion uses Hinnant's integer
`days_from_civil` / `civil_from_days` algorithms, so there is no floating-point
drift and negative epochs decompose canonically (`-1` → `1969-12-31T23:59:59Z`).
The reference values below were cross-checked against Python's `datetime`.

- `+iso8601Formatter` — `2026-10-25T12:34:56Z`; with a non-zero offset the
  suffix becomes `+08:00` / `-05:30`.
- `+dateOnlyFormatter` — `2026-10-25`; `+timeOnlyFormatter` — `12:34:56`.
- `+formatterWithPattern:` — token subset `yyyy`/`MM`/`dd`/`HH`/`mm`/`ss` plus
  literals; unrecognised letters are emitted literally, never guessed as tokens.
  A pattern that does not fit the fixed buffer is rejected (nil), not truncated.
- `-setUTCOffsetMinutes:` — the only timezone entry point (fixed offset).
- `-stringFromDate:` / `-dateFromString:` — the latter returns nil on invalid
  input (ObjC-aligned): out-of-range month, non-date text, trailing garbage,
  or an out-of-range clock.

Honest boundaries (see the header): no tzdata (UTC and fixed offsets only, no
DST or historical zones), no locale (month/weekday names, AM/PM, localized
digits all absent — ObjC relies on ICU), fractional seconds are parsed but
truncated, and formatting is whole-second.

Verified here:

| case | checked |
|------|---------|
| B1 | ISO 8601: `0`, `-1` (negative epoch), `1761395696` |
| B2 | date-only / time-only shapes, `3661 → 01:01:01` |
| B3 | fixed offset `+480` → `1970-01-01T08:00:00+08:00` |
| B4 | parse round-trips: `Z`, fractional seconds (truncated), `+08:00`, date-only, time-only |
| B5 | invalid input → nil: month 13, non-date text, trailing garbage, hour 25 |
| B6 | custom pattern `yyyy/MM/dd HH:mm` (format and parse) |

Output is deterministic (fixed epochs, never "now") — GOLDEN-MATCH line by line.
