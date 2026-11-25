# integ/fps-15: the eight branches of the "15 fps" build together

Branch `integ/fps-15`, from `feature/levels-on-demand` (53a0b1a), merges the
reviewed heads of:

| branch | what | switches (off by default unless said) |
|---|---|---|
| `fix/gpu-hang-review` | the gameplay loss of 2026-11-25 was a shader page fault ("MMU Fault"), not a timeout; DRED in release, GPU watchdog, driver entries in the device-removed report, mesh guard | `VICTORY_DRED`, `VICTORY_GPU_WATCHDOG` (on, 2000 ms, live), `VICTORY_GPU_PROGRESS`, `VICTORY_MESH_GUARD`, `VICTORY_SWEEP_QUARANTINE_MS` (live), `VICTORY_DRIVER_WATCH` |
| `fix-gpu-stalls` | the 1.7 fps stalls: remap lock, pipeline waits per first use, library saves | `VICTORY_TEXTURE_NO_WAIT` (live), `VICTORY_REMAP_BATCH`, `VICTORY_TEXTURE_SHORT_HOLD` (live), `VICTORY_PIPELINE_WAIT_EACH_MS` (live), `VICTORY_LIBRARY_SAVE_ASIDE`, `VICTORY_LIBRARY_ALT_NAME` |
| `perf/scratch-on-card-review` | lists' scratch chunks in GPU upload heaps; a line of our own card memory | `VICTORY_SCRATCH_ON_CARD` (live), `VICTORY_BUDGET_SLAB` |
| `perf/small-copies-on-card-review` | hot read-only buffer copies move to GPU upload heaps | `VICTORY_READONLY_COPIES_ON_CARD` (live), `VICTORY_READONLY_COPIES_MB` |
| `shader-indexed-registers` | indexed reads as select trees; build.rs cache-key fix | `VICTORY_SHADER_NO_ARRAYS` |
| `shader-memory-access` | fewer costs per access to title memory by address (fault note behind a branch, shared directory looks, hoisted strides, CBV rows read once) | `VICTORY_SHADER_FAST_MEM` |
| `shader/control-flow-review` | fault records behind one branch, unguarded lanes parked, wave64 paired form | `VICTORY_SHADER_FAST_FLOW` (`faults`, `parked`, `paired`, `1`) |
| `shader-correctness-batch` | leonida #1493/#978 (signed 64-bit literals), #966 (`ds_write2` equal offsets); sampler MaxLOD | rules on by default; `VICTORY_SAMPLER_LOD_ORDER` (live) |

Each branch's own notes are in `docs/perf/<branch>.md`.

## How the two shader branches were joined

`shader-memory-access` and `shader/control-flow-review` both rewrote
`Lower::memory` (`crates/victory-shader/src/emit/lower.rs`). Joined as:

- `memory_look(lo, hi, guard)`: the directory look. Lanes outside `guard`
  are parked (`FAST_FLOW` `parked`) only on the plain path (`memory`). With
  `FAST_MEM` the looks are shared between accesses (`memory_at`) and are made
  without a guard, so nothing is parked there.
- `memory_note`: with `FAST_MEM` the fault note is behind its own branch, at
  once. Without it, `FAST_FLOW` `faults` defers the records to one branch
  after the access (`flush_faults`), else every lane stores as before.
- `memory_smem(inst, b)` wraps `memory_smem_at(inst, b)` in `fast_memory`.

So with `VICTORY_SHADER_FAST_MEM=1`, `FAST_FLOW`'s `faults` and `parked` add
nothing more: both changes go after the same stores, and `FAST_MEM` already
takes them. Only `paired` would still change programs; it is left off in the
tests (its reviewer found it exact only on the in-step shortcut, and it may
be slower). The log lines of each still say what they did.

The test harness's two variants of a raw UAV with data (`RawUavData`,
`RawUavInit`) were the same thing; `RawUavData` is kept. A root UAV
(`Root::Uav`) is made in the UNORDERED_ACCESS state, the state the readback
transitions from.

## The two tests

Same build (`victory-f15.exe`), same cache folder
(`victory-f15.toml`, `G:\GTA VI\victory-f15-data`), same translator switches:

1. Bake: `victory translate --warm all` over the whole shader pack, then one
   game run with `VICTORY_DRED=1` that makes the pipelines (one phase of 240 s
   after the reference passage).
2. Real test: DRED off; live phases of 45 s: all on, scratch off, read-only
   copies off, both off, quarantine off, all on again.

Environment of both (on top of `tools/perf/play-lod.ps1`):

```
VICTORY_MESH_GUARD=1 VICTORY_GPU_PROGRESS=1 VICTORY_DRIVER_WATCH=1 VICTORY_SWEEP_QUARANTINE_MS=3000
VICTORY_TEXTURE_NO_WAIT=1 VICTORY_REMAP_BATCH=1 VICTORY_TEXTURE_SHORT_HOLD=1
VICTORY_LIBRARY_SAVE_ASIDE=1 VICTORY_LIBRARY_ALT_NAME=1 VICTORY_PIPELINE_WAIT_EACH_MS=100
VICTORY_SCRATCH_ON_CARD=1 VICTORY_BUDGET_SLAB=1 VICTORY_READONLY_COPIES_ON_CARD=1 VICTORY_READONLY_COPIES_MB=256
VICTORY_SHADER_NO_ARRAYS=1 VICTORY_SHADER_FAST_MEM=1 VICTORY_SHADER_FAST_FLOW=faults,parked
```
