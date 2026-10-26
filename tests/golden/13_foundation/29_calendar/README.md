# 29_calendar — NPCalendar + NPDateComponents

**NPCalendar** / **NPDateComponents**
(`include/Foundation/NPCalendar.hje/.jeti`, `NPDateComponents.hje/.jeti`) are the
`NSCalendar` / `NSDateComponents` ports, v1 scope: Gregorian decomposition and
construction, weekday, days-in-month, and day arithmetic.

Epoch ↔ civil conversion again uses Hinnant's integer algorithms (no
floating-point drift; negative epochs decompose canonically). Weekday follows
ObjC's default numbering, **1 = Sunday … 7 = Saturday**.

- `+currentCalendar` / `+calendarWithUTCOffsetMinutes:` — the timezone entry
  point is a fixed offset (no tzdata), so `+currentCalendar` is a UTC calendar.
- `-componentsFromDate:` — fills every field, including `weekday`.
- `-dateFromComponents:` — `year` is required (else nil); unset month/day
  default to 1 and unset clock fields to 0, matching ObjC's
  `NSDateComponents` semantics. Out-of-range values (month 13, day 32,
  hour 24, or a day past the month's length such as 2025-02-29) return nil —
  ObjC raises there instead.
- `-dayOfWeekFromDate:`, `-daysInMonth:year:` (century leap rule included),
  `-dateByAddingDays:toDate:`.
- `NPDateComponents` tracks a per-field "was it set" bitmask, so callers can
  distinguish an explicit 0 from an absent field.

Honest boundaries (see the headers): Gregorian only (no Buddhist/Chinese/
Hebrew identifiers; pre-1582 dates use the proleptic calendar), fixed offsets
only (no DST, no historical zones), no locale (`firstWeekday` is always
Sunday), and day arithmetic only — month/year addition needs an
overflow-day policy (`NSCalendarOptions`) that v1 does not model.

Verified here:

| case | checked |
|------|---------|
| C1 | `0` → `1970-01-01 00:00:00 wd=5`; `-1` → `1969-12-31 23:59:59 wd=4` |
| C2 | `1792931696` → `2026-10-25 12:34:56 wd=1` |
| C3 | `dateFromComponents:` round-trip → `1792931696` |
| C4 | leap rules: 2024-02=29, 2025-02=28, 2000-02=29, 1900-02=28 |
| C5 | weekday numbering (1 = Sunday) |
| C6 | `dateByAddingDays:` `+7` / `-1` |
| C7 | fixed offset `+480` → `1970-01-01 08:00:00` |
| C8 | invalid components → nil (month 13, 2025-02-29, missing year) |
| C9 | `NPDateComponents` has-bitmask |

All conversions were cross-checked against Python's `datetime`. Output is
deterministic — GOLDEN-MATCH line by line.
