# 30_regex — NPRegularExpression

**NPRegularExpression** (`include/Foundation/NPRegularExpression.hje/.jeti`) is
the `NSRegularExpression` port, implemented as a **self-contained backtracking
engine** with no ICU dependency (the plan explicitly allows a simplified engine
for v1, with the unsupported surface written down instead of silently
degrading).

Supported: literals, `.`, character classes `[abc]` / `[a-z]` / `[^...]`,
the escape classes `\d \w \s` and their negations, anchors `^` / `$`,
quantifiers `*` `+` `?` `{n}` `{n,}` `{n,m}` (greedy), alternation `|`,
capturing groups `(...)` and non-capturing `(?:...)`, and escapes `\.` `\*` …

Rejected at compile time (the factory returns nil, never a silent downgrade):
unclosed groups or classes, inverted ranges (`a{2,1}`), backreferences (`\1`),
lookaround / named groups / inline flags (`(?...)` other than `(?:`), lazy
quantifiers (`*?`), and patterns longer than the fixed buffer.

Honest boundaries: no `\p{...}` Unicode properties, no `\b` word boundary, no
capture-text extraction yet (only `-numberOfCaptureGroups`), and matching is
byte-oriented (no UTF-8 code-point classes). Backtracking is bounded by a step
budget: a catastrophically backtracking pattern is reported as **not matching**
rather than hanging.

Two bugs the probe caught while building this:

1. **Anchors never matched.** The quantifier enumerator treated any zero-width
   match as "no progress" and dropped it entirely, so a pattern starting with a
   zero-width atom (`^abc`) could never satisfy its minimum repetition count.
   A single zero-width match is now counted (without growing further).
2. `sizeof(((NPRegularExpression *)0)->_pattern)` — the buffer-capacity check
   was written as a member access on a `sizeof` result, which does not compile.

Verified here:

| case | checked |
|------|---------|
| D1 | literals, `.`, `[a-z]+`, `[^0-9]+` |
| D2 | `\d+`, `\w+`, `\s` |
| D3 | `^abc` / `abc$` / `^abc$` (zero-width anchors) |
| D4 | `ab*`, `ab+`, `ab?`, `a{2,3}`, `a{2}` (greedy) |
| D5 | `cat\|dog`, `(ab)+`, `(?:ab)+`, `(a\|b)+c`, capture-group counting |
| D6 | invalid patterns → nil |

Output is deterministic — GOLDEN-MATCH line by line.
