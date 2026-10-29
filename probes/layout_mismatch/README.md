# Cross-TU layout-mismatch probe (R3 / `__sig` startup check)

Purpose: prove the runtime vtable-layout guard still fires **and still prints the
same diagnostic** after the method-name table was made shared (binary-size work).

Build and run:

```bash
../../target/release/jetic main.jeti lib.jeti -I ../../include -o probe
./probe          # must abort (SIGABRT) at load time
```

Expected: exit code 134, and the message ends with

```
Methods known to this TU:
  <the TU's whole method list> | class Thing | tu main.jeti
```

## Why this shape triggers the check

`collect_public_methods` (`crates/jetic/src/pipeline.rs`) treats a selector as
public when it is declared in an `#import`ed `@interface`. The signature segment
is then `manifest ∩ public` (`vtable_sig_segment`, `crates/codegen/src/codegen.rs`).

* `lib.jeti` imports the Foundation umbrella, so every Foundation selector is
  public → its segment is (almost) the whole `.slots` manifest.
* `main.jeti` imports only `shared_a.hje`, so its public set is just `count` —
  and `count` happens to be a manifest selector (line 51 of
  `target/foundation/libjetifoundation.slots`) → its segment is `{count}`.

The two TUs therefore compile different `struct jeti_vtable` layouts for the same
class, while the linker weak-merges the instances. `lib.jeti` owns `Thing`, so its
strong instance wins and its `__sig` (`5b7de60a9424d801`) is what `main.jeti`'s
constructor reads back — a mismatch against its own `b1e5e28e4479a274`.

## Gotcha: the `.slots` seed usually hides this

`find_slots_seed` (`crates/jetic/src/main.rs`) auto-applies
`libjetifoundation.slots` to every compile that links the archive, and the segment
is then `manifest ∩ public`. Two TUs that both import the umbrella have the *same*
public set, so adding a brand-new selector to one of them does **not** change its
segment — the seed is exactly what stops that from being a false alarm. A probe
that adds a selector unknown to the manifest therefore links and runs cleanly; the
mismatch above needs a manifest selector whose *publicity* differs.

## Known limitation this probe sidesteps

A client TU that does not import the umbrella cannot allocate the class: `[[Thing
alloc] init]` lowers to the invalid `Thing->alloc->init` (the class-object method
send path needs the Foundation declarations). The check runs in a load-time
constructor, so `main` never executes anyway — the probe keeps `main` trivial.
