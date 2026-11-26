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

## Results (2026-11-26, RTX 5080, victory-f15.exe)

Bake: the offline warm-up translated 25,751 of 25,985 shaders in 127 s on
24 threads (234 do not translate). The leonida rules met no title shader
(0 signed 64-bit literals, 0 `ds_write2` at one address). The bake run (DRED
on, pipelines being made) ran without a loss: 5.7 fps median, light right
(the owner watched it).

Real test (second launch, same cache, DRED off), live phases of 45 s, past
the cutscene into the cell (the scene moves, so the draws per frame differ):

| phase | frames | fps median | min-max | ms | preparing | card busy |
|---|---|---|---|---|---|---|
| all on | 47 | 9.3 | 0.7-30.2 | 232 | 61.7 | 40% |
| scratch off | 13 | 10.8 | 10.3-11.7 | 92 | 42.8 | 84% |
| read-only copies off | 12 | 9.5 | 3.9-9.9 | 121 | 55.6 | 78% |
| both off | 12 | 9.7 | 4.9-10.6 | 116 | 53.0 | 73% |
| quarantine off | 12 | 11.0 | 9.1-11.9 | 94 | 50.4 | 67% |
| all on again | 12 | 11.05 | 2.6-11.4 | 116 | 49.5 | 70% |

No loss, no watchdog warning. No measurable gain: 9.5-11 fps as before.
The light was wrong in this run: the cell lit only by the sun's patches,
no ambient light. What differs from the bake run: 2,365 pipelines loaded from
the library and 1,800 started early (`started_early`); no store was refused
and none went under a second name, so `VICTORY_LIBRARY_ALT_NAME` is not
involved. Both runs dropped ~350 draws a frame for want of a host pipeline
(the unimplemented pipeline method 0x218), the same in both.

For the A/B: `VICTORY_LIBRARY_OFF=1` (the library on disk neither read nor
written) and `VICTORY_PIPELINE_EARLY=0` (no early start), both read once.

## Test 3: with the AMD port (victory-f15b.exe, 2026-11-26)

Settings: test 2's, without `VICTORY_READONLY_COPIES_ON_CARD`, plus
`VICTORY_LIBRARY_OFF=1`, card copies from 64 KiB
(`VICTORY_CARD_BUFFERS_MIN_KB=64`) with `VICTORY_CARD_ALL=1`, and
`VICTORY_SEND_OWN`, `CARD_HASH_LEN`, `CARD_COMPARE_EIGHT`, `KEEP_FREED`,
`WAIT_RESIDENT`, `VIEW_COPY`, `TEXTURE_BACK` on; `send_own` and
`keep_freed` toggled live.

| phase | fps median | min-max | card busy |
|---|---|---|---|
| port all on (pipelines compiling) | 4.9 | 0.5-30 | 36% |
| send_own off | 7.7 | 4.1-8.9 | 78% |
| send_own on | 8.25 | 6.8-9.2 | 87% |
| keep_freed off | 7.8 | 2.7-8.8 | 77% |
| scratch off | 8.35 | 7.0-9.8 | 88% |
| all on again | 8.3 | 2.5-9.1 | 82% |

The picture: black at first, then a slow flicker mostly black, then a faster
flicker (the owner watching); every screenshot black. The black picture of
64 KiB copies stays with `SEND_OWN` and `CARD_ALL`, on or off: they are not
its cause, or not all of it. Slower than test 2 (8.3 against 9.5-11 fps).
Compiling every pipeline in the run (`VICTORY_LIBRARY_OFF=1`) took about
10 minutes at 5 fps before the queue was empty.

The library refused stores under a name already there: 21 in the bake run,
5 in test 2, 28 here (with the library off, so within one run). No load
under a second name in test 2. `StorePipeline` refuses any name already
stored, so these are most likely one host pipeline made twice (two title
pipelines with the same host description), not two descriptions under one
name. The light question stays open: this run's picture was black.

## Test 4: test 2's settings plus `VICTORY_LIBRARY_OFF=1` (2026-11-26)

Same build as test 3 (`victory-f15b.exe`), the AMD port's switches all off,
read-only copies on, card copies from 512 KiB. About 10 minutes of
compiling at 5 fps first.

| phase | fps median | min-max | draws | card busy |
|---|---|---|---|---|
| all on (pipelines compiling) | 5.1 | 0.5-30 | 2153 | 36% |
| scratch off | 9.4 | 8.3-10.1 | 4441 | 79% |
| read-only copies off | 8.7 | 7.4-9.3 | 4399 | 80% |
| both off | 8.7 | 7.9-10.3 | 4495 | 79% |
| quarantine off | 8.95 | 7.7-9.7 | 4471 | 78% |
| all on again | 9.05 | 8.0-9.5 | 4583 | 78% |

The light is right (the owner watching, and the screenshots): with every
pipeline made in the run, nothing read from the library and nothing started
early, the picture is the bake run's. So what breaks the light in a second
launch is the library read back or the early start (both are off with
`VICTORY_LIBRARY_OFF=1`). No loss. 8.7-9.4 fps with the light right, in the
cutscene (test 2's 9.5-11 fps were in the cell with the light wrong).

## Tests 5-7: what breaks the light on a second launch (2026-11-26)

All with test 2's settings and `victory-f15b.exe` (AMD port switches off).

| test | library | early start | per-use cap | draws dropped for want of a pipeline | light | other |
|---|---|---|---|---|---|---|
| 2 | read | on | 100 ms | some | wrong | |
| 4 | off | off | 100 ms | many (compiling) | right | all right |
| 5 | read | off (`VICTORY_PIPELINE_EARLY=0`) | 100 ms | some | right | a face missing, poor shadows, artifacts at the bottom |
| 6 | read | on | none, 2 s budget | 0 | wrong | blocky shadows, blue garbage at the bottom, textures missing |
| 7 | read | off | none, 2 s budget | 0 | right | faces and a few textures missing |

Test 7's fps (cutscene into the cell): 6.8 (loading), 7.95, 8.35, 8.4, 8.9,
9.45 by phase. Test 5 (stopped early): 8.8-9.6. Test 6 (stopped early):
8.6, 7.75.

So the early start breaks the light, and not through draws dropped while
pipelines are made: test 6 dropped none and the light was wrong; test 7
dropped none and it was right. (A read-only review had proposed that early
start lets one-time load passes run that otherwise were dropped, the list
`early-pipelines.txt` noting requests rather than runs since d279e69; test 7
refutes it.) The library read back without early start still loses faces and
a few textures; only test 4, with every pipeline compiled in the run, was
right in all. Leading guess, not measured: card memory. Early start makes
~1,800 pipelines the scene may not use, and the driver may place a 250 MB
library's code in video memory; less room for textures broke the light
before (the 3072 MB budgets).

Next: compile in the run, faster (more pipeline workers, the driver's own
shader cache), or read back only the kinds of pipelines that are right.

## The night of 2026-11-26

Measurements, builds and decisions are in `docs/perf/nuit-2026-11-26.md`:
- **fps:** the card is the limit at about 10-10.6 fps on the RTX 5080. The best set, night build 4, is run by `tools/perf/play-night.ps1`.
- **Picture:** right with Thibault's clean set (PR #9). The translator switches `NO_ARRAYS`, `FAST_MEM` and `FAST_FLOW` stay off.
- **Taken into this branch:** fixes from fxceless (#1210, #911/#1206, #1185, #1241, a35c557, #1260, #1264, #1261, #1237/#1245, #1227) and avatarmanz (#87, #114 with 4243a96a).
- **Kept on their own branches:** `port/bvh-builder-nvidia`, `port/cascade-shadows`, `fix/bottom-blocks` (in test), `port/coolfox-gpu` and `port/coolfox-cpu` (in progress).
