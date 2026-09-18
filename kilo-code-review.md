# Code Review: cosmic-media-applet

## Summary
MPRIS-only COSMIC applet controlling Brave/YouTube via D-Bus. Stack: libcosmic 1.0 (git), tokio full, zbus 5.19, no mpris crate.

---

## BLOCKER — FIXED

**[adapter.rs:101] - `block_on` in async context** — FIXED
- **Fixed:** Rewrote `is_available()` to be fully async without `block_on`.

**[adapter.rs:73] - `extract_string` mishandles `xesam:artist` Array** — FIXED
- **Fixed:** Removed Array handling from `extract_string`; `metadata_to_track` handles artist array explicitly by joining all elements.

---

## MAJOR — FIXED

**[manager.rs:53-77] - Unnecessary polling interval** — FIXED
- **Fixed:** Removed 3-second `interval.tick()`; scan now relies solely on `NameOwnerChanged` signal.

**[manager.rs:136-135] - Double fetch in `do_scan`** — FIXED
- **Fixed:** Caches state/track during selection loop; reuses for active source emission.

**[manager.rs:17] - `_scan_handle` never joined on drop** — FIXED
- **Fixed:** Added `Drop` impl that aborts `_scan_handle`; changed field to `Option<JoinHandle>`.

**[main.rs:127,136,145] - Route errors mapped to wrong event** — FIXED
- **Fixed:** Errors now emit `MediaEvent::StateChanged(Stopped)` instead of `SourceListChanged`.

**[adapter.rs:99-102] - `is_available` creates Proxy per call** — PARTIALLY FIXED
- **Fixed:** Removed `block_on`, but still creates Proxy per call. NEEDS CONTEXT: Consider caching liveness via `NameOwnerChanged` in manager.

**[manager.rs:187-197] - Exposing internal `RwLock` references** — FIXED
- **Fixed:** Removed `sources()` and `active_source()` methods; use `*_snapshot()` / `*_cloned()` instead.

---

## MINOR — FIXED

**[manager.rs:28] - `new_empty()` spawns noop task** — FIXED
- **Fixed:** `_scan_handle` now `Option`, set to `None` in `new_empty()`.

**[adapter.rs:91] - `source_id` empty in `metadata_to_track`** — FIXED
- **Fixed:** `metadata_to_track` now accepts `source_id` parameter; constructs `TrackInfo` with correct value.

**[main.rs:169-195] - Duplicate track/state rendering logic** — FIXED
- **Fixed:** Added `apply_track_state` helper on `MediaApplet`; used by `UpdateState` handler.

**[manager.rs:110-118] - Selection priority logic unclear** — UNCHANGED (NEEDS CONTEXT)
- See NEEDS CONTEXT below.

---

## SUGGESTION — FIXED / UNCHANGED

**[adapter.rs:69-76] - `extract_string` recursive box handling** — FIXED
- **Fixed:** Removed dead `Value::Value` branch.

**[manager.rs:89-136] - `do_scan` does too much** — UNCHANGED
- Still does discovery, selection, fetch, emission. Could split but not blocking.

**[main.rs:51-75] - `BroadcastSubscription` recipe** — UNCHANGED
- Works; verbose but functional.

---

## NEEDS CONTEXT

**[manager.rs:107-118] - Active source selection policy**
- **Question:** What if multiple players `Playing`? First-found is arbitrary (DBus ListNames order). Should prefer most recently active? User-preferred?

**[manager.rs:129-135] - Event emission on scan**
- **Question:** `SourceListChanged` emitted every scan even if list unchanged. UI may re-render unnecessarily. Diff before emit?

**[adapter.rs:39-64] - Watch task error handling**
- **Question:** Task silently returns on proxy/signal errors. Should log and resubscribe? Current: player disappears → no more updates.

**[adapter.rs:101] - `is_available` Proxy-per-call**
- **Question:** Still creates new Proxy per call. Should manager track liveness via `NameOwnerChanged` and expose cached bool?

---

## What's Good

- **Architecture:** Clean `MediaSource` trait separation; `MprisAdapter` encapsulates D-Bus details.
- **Concurrency:** `Arc<RwLock<>>` + interior mutability; `scan()` takes `&self` correctly.
- **Signals:** Uses `PropertiesChanged` for track/state (adapter watch task) — correct per AGENTS.md.
- **Error types:** `AppError` with `thiserror`; `anyhow` for manager internal.
- **No `block_on`** anywhere.
- **zbus 5.x** + `zvariant 5` — matches libcosmic requirement.