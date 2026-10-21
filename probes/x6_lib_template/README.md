# probe: precompiled library + generic specialization + frozen layout

What this pins down, in two halves:

1. **Template gap**: a client that sees only declarations and links a
   precompiled archive cannot get a method BODY for a specialization the archive
   was not built with. Codegen emits an empty weak vtable stub, the link
   succeeds, and the first dispatch jumps to NULL. Fixed by the archive shipping
   `lib<name>.templates` (R5).
2. **Layout gap**: a client that imports headers of its OWN adds selectors the
   archive never saw, which shifts a library class's slot indices and aborts at
   startup (`vtable layout mismatch across translation units`). Fixed by the
   archive shipping `lib<name>.slots`, which the driver inherits read-only as
   this build's frozen prefix (R6).

## Files

| file | role |
| --- | --- |
| `main_foundation.jeti` | Foundation-only client using `NPMutableArray<NPString *>` (template gap alone) |
| `Box.hje` / `Box.jeti` / `main.jeti` | library-owned template (`Box<T>`) plus a client that ALSO declares its own class (both gaps) |
| `libBox.templates` | template manifest shipped beside `libBox.a` |

## Run

```bash
# 1. template gap only — the Foundation archive ships both manifests itself
./target/debug/jetic -L target/foundation -ljetifoundation \
    probes/x6_lib_template/main_foundation.jeti -o /tmp/x6f && /tmp/x6f
# → first=hi count=1

# 2. build the probe-local archive with the Foundation layout as its base, so
#    the client's own class cannot move Box's slots, and ship the template source
cp target/foundation/libjetifoundation.slots /tmp/boxseed.slots
./target/debug/jetic -rewrite-jeti probes/x6_lib_template/Box.jeti \
    -I probes/x6_lib_template -I include -I include/Foundation \
    --slots /tmp/boxseed.slots -o /tmp/Box.c
clang -c -w /tmp/Box.c -o probes/x6_lib_template/Box.o -I include
ar rcs probes/x6_lib_template/libBox.a probes/x6_lib_template/Box.o

./target/debug/jetic probes/x6_lib_template/main.jeti \
    -L probes/x6_lib_template -lBox -L target/foundation -ljetifoundation \
    -o /tmp/x6b && /tmp/x6b
# → boxed=hi
```

Reverse controls (each one reproduces a distinct failure):

| remove | result |
| --- | --- |
| `target/foundation/libjetifoundation.templates` | links, exits 139 (NULL stub) |
| `target/foundation/libjetifoundation.slots` | aborts at startup, rc=134 (R3) |
| `probes/x6_lib_template/libBox.templates` | links, exits 139 (`Box<NPString *>` never instantiated) |

The client passes **no layout flag at all** in both runs: discovery of
`lib<name>.slots` / `lib<name>.templates` next to the archive is the feature
under test.
