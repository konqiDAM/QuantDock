# Memory Churn Reduction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut the per-tick allocation churn that makes Windows commit more heap pages, then prove it with a flat 10-minute plateau.

**Architecture:** Replace the two hottest `sysinfo` refreshes in the overlay with a zero-allocation `GlobalMemoryStatusEx` read and a 5 s process refresh; cache `server::status()` for 3 s in both binaries; reuse one `monitor_lhm::Reader` for the manager's probes; halve repaint rates. No new crates.

**Tech Stack:** Rust 2024, egui/eframe 0.37, sysinfo 0.40, ureq 3.5, windows 0.62.

**Spec:** `docs/superpowers/specs/2026-11-15-log-console-sensor-polish-design.md` (memory goal), plus the heap trace captured on 2026-11-15:
`%TEMP%\opencode\heap-prestart-merged2.etl` and `heap-total-stacks.txt`.

## Evidence

A 2-minute xperf heap trace with both release binaries shows all allocations are freed
(live heap < 320 KB per process), but the churn is large:

| Process | Top allocating stack | Allocations | Total |
|---|---|---|---|
| monitor | `sysinfo::refresh_memory_specifics` → `K32GetPerformanceInfo` → `LocalAlloc` | 73,778 | 149.7 MB |
| monitor | `sysinfo::refresh_processes` → `ProcessInner::update` | 195 | 54.3 MB |
| monitor | `RtlpWnfNotificationThread` (Windows) | 23,469 | 48.4 MB |
| monitor | OpenGL thread initialization | 7,712 | 31.2 MB |
| quantdock | `RtlpWnfNotificationThread` (Windows) | 23,783 | 53.4 MB |
| quantdock | `epaint::Tessellator::tessellate_text` | 7,712 | 31.2 MB |
| quantdock | `server::status` → `ureq` `LazyBuffers` | 13,917 | 32.5 MB |

The Windows heap commits pages as this churn continues and does not return them.
`HeapCompact` and mimalloc were measured as no-ops. The WNF thread has no application
frames and is the expected floor; everything else is reducible.

## Global Constraints

- No new crates.
- Windows-only, as today.
- All HTTP and LibreHardwareMonitor work stays off the UI thread.
- No change to the acceptance test: after a 2-minute warm-up, growth ≤ 2 MB/min and no
  monotonic climb across the final 5 minutes.
- No new user-facing settings; sampling cadences are internal constants.
- Do not hide the number with working-set trimming. Fix the allocation rate or report
  the remaining OS-side floor honestly.

## Review Focus

- `GlobalMemoryStatusEx` fails or a machine has no page file: RAM/swap fields are absent
  or zero, never negative or a panic (Task 1).
- The first sampler tick must populate `llama_ram`/`uptime`; a five-second stale value is
  acceptable, an empty one is not (Task 2).
- A model load or stop must appear within the 3 s status cache (Task 3).
- Two overlapping LHM probes must serialize on the shared `Reader`, not deadlock or block
  the UI (Task 4).
- Halved repaint rates must still update the overlay at the configured interval and the
  manager status within 2 s (Task 5).

---

### Task 1: Zero-allocation RAM and swap read

**Files:**
- Create: `src/sysmem.rs`
- Modify: `src/lib.rs`, `src/monitor_sample.rs`, `Cargo.toml`

**Interfaces:**
- Produces: `pub struct Mem { pub used_mb: f64, pub total_mb: f64, pub swap_used_mb: f64, pub swap_total_mb: f64 }`
- Produces: `pub fn read() -> Option<Mem>`
- `monitor_sample.rs` consumes `crate::sysmem::read()` and no longer calls
  `sys.refresh_memory()`, `used_memory()`, `total_memory()`, `used_swap()` or
  `total_swap()`.

- [ ] **Step 1: Write the failing tests**

In `src/sysmem.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::SystemInformation::MEMORYSTATUSEX;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn computes_used_and_swap_from_memory_status() {
        let s = MEMORYSTATUSEX {
            ullTotalPhys: 32 * GB,
            ullAvailPhys: 8 * GB,
            ullTotalPageFile: 40 * GB,
            ullAvailPageFile: 4 * GB,
            ..Default::default()
        };
        let m = from_status(&s);
        assert_eq!(m.total_mb, 32.0 * 1024.0);
        assert_eq!(m.used_mb, 24.0 * 1024.0);
        assert_eq!(m.swap_total_mb, 8.0 * 1024.0);
        assert_eq!(m.swap_used_mb, 4.0 * 1024.0);
    }

    #[test]
    fn no_page_file_means_zero_swap() {
        let s = MEMORYSTATUSEX {
            ullTotalPhys: 16 * GB,
            ullAvailPhys: 4 * GB,
            ullTotalPageFile: 16 * GB,
            ullAvailPageFile: 12 * GB,
            ..Default::default()
        };
        let m = from_status(&s);
        assert_eq!(m.swap_total_mb, 0.0);
        assert_eq!(m.swap_used_mb, 0.0);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --all-targets sysmem`

Expected: compile error, `sysmem` module and `from_status` not found.

- [ ] **Step 3: Implement `src/sysmem.rs`**

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct Mem {
    pub used_mb: f64,
    pub total_mb: f64,
    pub swap_used_mb: f64,
    pub swap_total_mb: f64,
}

#[cfg(windows)]
pub fn read() -> Option<Mem> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut s = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut s).ok()? };
    Some(from_status(&s))
}

#[cfg(not(windows))]
pub fn read() -> Option<Mem> {
    None
}

fn from_status(s: &windows::Win32::System::SystemInformation::MEMORYSTATUSEX) -> Mem {
    // sysinfo's swap is page-file usage: commit_total minus physical total.
    // used = total_phys - avail_phys
    // swap_total = total_page - total_phys, saturating
    // commit_total = total_page - avail_page
    // swap_used = commit_total - total_phys, saturating
}
```

Add `pub mod sysmem;` to `src/lib.rs` and `"Win32_System_SystemInformation"` to the
`windows` features in `Cargo.toml`.

In `src/monitor_sample.rs`, replace the `sys.refresh_memory()` block (the `ram`,
`ram_pct` and `swap` inserts) with the block below. The existing `const MB` above it
stays and is reused:

```rust
if let Some(mem) = crate::sysmem::read() {
    sample.fields.insert("ram", Value::UsedTotal(mem.used_mb, mem.total_mb));
    if mem.total_mb > 0.0 {
        sample.fields.insert("ram_pct", Value::Num(mem.used_mb / mem.total_mb * 100.0));
    }
    if mem.swap_total_mb > 0.0 {
        sample.fields.insert("swap", Value::UsedTotal(mem.swap_used_mb, mem.swap_total_mb));
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --all-targets sysmem` — expected: 2 passed.

Run: `cargo test --all-targets` — expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/lib.rs src/sysmem.rs src/monitor_sample.rs
git commit -m "perf: read RAM and swap without sysinfo allocation"
```

---

### Task 2: Refresh the process list every five seconds

**Files:**
- Modify: `src/monitor_sample.rs`

**Interfaces:**
- Produces: `fn refresh_due(last: Instant, now: Instant, every: Duration) -> bool`
- `PROCESS_REFRESH: Duration = Duration::from_secs(5)`.
- Change the module import to `use std::time::{Duration, Instant};`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/monitor_sample.rs`:

```rust
#[test]
fn process_refresh_is_due_after_five_seconds() {
    let now = Instant::now();
    assert!(refresh_due(now - Duration::from_secs(5), now, PROCESS_REFRESH));
    assert!(!refresh_due(now - Duration::from_secs(4), now, PROCESS_REFRESH));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --all-targets process_refresh`

Expected: compile error, `refresh_due` and `PROCESS_REFRESH` not found.

- [ ] **Step 3: Implement the throttle**

```rust
const PROCESS_REFRESH: Duration = Duration::from_secs(5);

fn refresh_due(last: Instant, now: Instant, every: Duration) -> bool {
    now.duration_since(last) >= every
}
```

Before the sampler loop: `let mut last_processes = Instant::now();` and
`let mut refresh_processes_now = true;`. The boolean forces the first refresh without
subtracting a duration from a fresh `Instant` (which can underflow).

Inside the existing `if shows(&snapshot, PROCESS_FIELDS)` block:

```rust
let now = Instant::now();
if refresh_processes_now || refresh_due(last_processes, now, PROCESS_REFRESH) {
    refresh_processes_now = false;
    last_processes = now;
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
}
// then read the cached process list exactly as today
```

The first tick refreshes; the next four ticks read the cached entries.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --all-targets process_refresh` — expected: 1 passed.

Run: `cargo test --all-targets` — expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/monitor_sample.rs
git commit -m "perf: refresh the process list every five seconds"
```

---

### Task 3: Cache `server::status` for three seconds

**Files:**
- Modify: `src/server.rs`

**Interfaces:**
- `pub fn status() -> Option<Status>` keeps its signature.
- `fn status_uncached() -> Option<Status>` holds the current body.
- `const STATUS_TTL: Duration = Duration::from_secs(3)`.
- `fn fresh(at: Instant, now: Instant, ttl: Duration) -> bool`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/server.rs` (add `use std::time::Instant;` there,
because the module top only imports `Duration`):

```rust
#[test]
fn status_cache_expires_after_three_seconds() {
    let now = Instant::now();
    assert!(fresh(now - Duration::from_secs(2), now, STATUS_TTL));
    assert!(!fresh(now - Duration::from_secs(3), now, STATUS_TTL));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --all-targets status_cache`

Expected: compile error, `fresh` and `STATUS_TTL` not found.

- [ ] **Step 3: Implement the cache**

Rename the existing `status()` body to `status_uncached()`. Add:

```rust
const STATUS_TTL: Duration = Duration::from_secs(3);

fn fresh(at: Instant, now: Instant, ttl: Duration) -> bool {
    now.duration_since(at) < ttl
}

pub fn status() -> Option<Status> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, Option<Status>)>> = Mutex::new(None);

    {
        let guard = match CACHE.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        if let Some((at, cached)) = &*guard {
            if fresh(*at, Instant::now(), STATUS_TTL) {
                return cached.clone();
            }
        }
    }

    // Do not hold the cache lock across the HTTP call.
    let value = status_uncached();
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    *guard = Some((Instant::now(), value.clone()));
    value
}
```

`props_cached` stays at 10 s and is still used by `status_uncached`.

- [ ] **Step 4: Verify manually**

Run: `cargo build --release`.

Start a model; `serving` appears within 3 s. Stop it; `server down` appears within 3 s.
The log console and model cards still update.

- [ ] **Step 5: Run the suite**

Run: `cargo test --all-targets` — expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/server.rs
git commit -m "perf: cache server status for three seconds"
```

---

### Task 4: Reuse one LHM reader in the manager probe

**Files:**
- Modify: `src/bin/quantdock.rs` (`lhm_probe`)

**Interfaces:**
- `fn lhm_probe(path: Option<&str>) -> LhmProbe` keeps its signature.
- The shared reader is a `OnceLock<Mutex<quantdock::monitor_lhm::Reader>>`.

- [ ] **Step 1: Implement the shared reader**

Inside `lhm_probe`:

```rust
use std::sync::{Mutex, OnceLock};
static READER: OnceLock<Mutex<quantdock::monitor_lhm::Reader>> = OnceLock::new();
let reader = READER.get_or_init(|| Mutex::new(quantdock::monitor_lhm::Reader::new()));
let mut guard = match reader.lock() {
    Ok(g) => g,
    Err(e) => e.into_inner(),
};
if let Some(data) = guard.read() {
    return LhmProbe {
        state: LhmState::Connected,
        ids: data.all.keys().cloned().collect(),
    };
}
```

Keep the existing `InstalledNotRunning` / `Missing` fallback. Do not move the reader
into the UI thread.

- [ ] **Step 2: Run the suite**

Run: `cargo test --all-targets` — expected: all pass.

- [ ] **Step 3: Verify manually**

With LibreHardwareMonitor running, the manager still reports `connected` and the
missing-sensor marker still updates. Stop LHM; the state becomes `InstalledNotRunning`
after the next probe.

- [ ] **Step 4: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "perf: reuse one LHM reader for manager probes"
```

---

### Task 5: Halve the repaint rates

**Files:**
- Modify: `src/bin/monitor.rs`, `src/bin/quantdock.rs`

**Interfaces:** none.

- [ ] **Step 1: Change the overlay**

In `src/bin/monitor.rs`, change `request_repaint_after(Duration::from_millis(500))` to
`Duration::from_millis(1000)`.

- [ ] **Step 2: Change the manager**

In `src/bin/quantdock.rs`, change `request_repaint_after(Duration::from_millis(1000))` to
`Duration::from_millis(2000)`.

- [ ] **Step 3: Build and verify manually**

Run: `cargo build --release`.

The overlay still updates at the default 1 s sampling interval; the manager's status
changes within 2 s. Dragging and resizing remain responsive.

- [ ] **Step 4: Commit**

```bash
git add src/bin/monitor.rs src/bin/quantdock.rs
git commit -m "perf: halve the repaint rates"
```

---

### Task 6: Measure the ten-minute plateau and record the result

**Files:**
- Modify: `docs/superpowers/plans/2026-11-15-memory-churn-reduction.md` (add a Result section)

**Interfaces:** none.

- [ ] **Step 1: Run the full suite and release build**

Run: `cargo test --all-targets` — expected: all pass.

Run: `cargo build --release` — expected: clean.

- [ ] **Step 2: Run the plateau**

Start the release manager and overlay with sensors enabled at the current defaults.
Sample private commit and working set every 30 s for 10 minutes.

Pass: after the first 2 minutes, growth ≤ 2 MB/min and no monotonic climb across the
final 5 minutes.

- [ ] **Step 3: Record the numbers**

Append a `## Result` section to this plan with the per-sample table, the growth rate for
both processes, and a pass/fail line. If it passes, state that the branch is ready to
merge. If it fails, stop: do not hide the remainder, and name the next largest stack
from the trace. The remaining suspects, in order, are `refresh_cpu_usage` every tick,
`Disks::new_with_refreshed_list()` when `disk_free` is visible (not in the current rows),
and the per-tick `nvidia-smi` spawn. Do not add fixes for them before the measurement
says they matter.

- [ ] **Step 4: Commit, merge, push**

```bash
git add docs/superpowers/plans/2026-11-15-memory-churn-reduction.md
git commit -m "docs: record the memory plateau result"
```

Then merge this branch to `main` using the finishing-a-development-branch skill, push
`main`, and confirm the `build` CI workflow is green. If the plateau failed, do not merge:
report the numbers and the next largest stack instead.
