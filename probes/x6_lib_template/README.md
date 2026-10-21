# probe: precompiled library + generic specialization

What this pins down: a client that sees only declarations and links a
precompiled archive cannot get a method BODY for a specialization the archive
was not built with. Codegen emits an empty weak vtable stub, the link succeeds,
and the first dispatch jumps to NULL.

## Files

| file | role |
| --- | --- |
| `main_foundation.jeti` | **the acceptance**: Foundation-only client using `NPMutableArray<NPString *>` |
| `Box.hje` / `Box.jeti` / `main.jeti` | a library-owned template (`Box<T>`) plus a client that ALSO declares its own class |

## Run

```bash
# Foundation ships target/foundation/libjetifoundation.templates next to the
# archive (tools/build-foundation-lib.sh writes it), so this just works:
./target/debug/jetic -L target/foundation -ljetifoundation \
    probes/x6_lib_template/main_foundation.jeti -o /tmp/x6f && /tmp/x6f
# → first=hi count=1
```

Without the manifest (`mv` it away) the same program links and exits 139.

## Known separation: `main.jeti` (own class) is NOT part of #3

`main.jeti` adds `@interface Box<T>` in a header the client imports, which puts
Box's selectors into the client's PUBLIC vtable segment while the archive's TUs
never saw them → the R3 startup check aborts
(`vtable layout mismatch across translation units`). That is the pre-existing
segment rule for precompiled-Foundation clients (`__sig` covers the public
segment), not the template gap: the same program aborts with or without the
manifest. The template mechanism itself is proven by `main_foundation.jeti`
and by `tests/multi_tu/18_lib_template/`, where the library is a probe-local
archive and both sides see the same header set.
