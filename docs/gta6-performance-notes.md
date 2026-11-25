# GTA VI frame rate under VIctory: findings so far (2026-11-25)

Notes from dkdknight's test series, branch `feature/watch-batch` of
`dkdknight/victory-fork`. Written with the help of an AI assistant (Claude
Code); every number is from our own runs. No game data is in this
repository: shader hashes below are only the names the logs print.

Setup: GTA VI (Series X build 1.0.2.1), RTX 5080 (16 GB), Windows 11,
display mode 1920x1080, `VICTORY_RELAY=2 VICTORY_LATE_WAIT=3
VICTORY_WIDE_BACK=1 VICTORY_CARD_BUFFERS_MIN_MB=1`. Reference scene: one
passage of the opening cutscene with about 4,950 draws a frame.

## Where it stands

| | Before this series | Best settings now |
|---|---|---|
| Full picture, `--console series-x` | ~3.5 fps | ~6.5 fps |
| Indirect mesh draws on the card | 137 ms/frame | ~22-25 ms/frame |
| All title commands on the card | ~200 ms/frame | ~85-100 ms/frame |
| `--console series-s` | lost the device while loading | ~8.6 fps, black screen |

The title itself is not the limit: with every draw, dispatch, refresh,
copy-back and upload dropped (`VICTORY_BARE=127`) it runs at 30.0 fps in
the open world. 30 fps with the picture needs under 33 ms of card work a
frame; a PIX capture of one frame shows 117-136 ms.

Best settings: `tools/perf/play-best.ps1` (paths are local ones; adapt).

## What was found

1. **The global UAV barrier before every indirect draw was the largest
   single cost.** About 5,000 pipeline drains a frame. With one-record
   indirect mesh draws going through an amplification stage
   (`VICTORY_INDIRECT_AMPLIFY=1`: the stage reads and bounds the record on
   the card, then `DispatchMesh`) and the barrier recorded only after a
   shader that may have written (`VICTORY_AMPLIFY_WAIT=auto`, about 1% of
   the draws), lists in flight on the card went from 86 ms to ~25 ms a
   frame. `VICTORY_CLAMP_WAIT=auto` does the same for the clamp pass of the
   other indirect commands (no measurable gain by itself).
2. **Measurement trap.** The indirect arguments are written by the title's
   compute shaders (GPU culling). With compute dropped the mesh draws ask
   for 0-127 groups a frame instead of 15,000-21,000, so a run without
   compute says nothing about geometry cost.
3. **There is no per-command floor.** The PIX capture gives medians of
   1.2 µs for `DispatchMesh`, 0.2 µs for `Dispatch`, 3.5 µs for
   `ExecuteIndirect`. Our own timestamp timing (`VICTORY_GPU_TIMING`) had
   suggested 5-7 µs each; that was an artefact. About 80 commands of
   18,000 hold more than 80 ms.
4. **(Withdrawn 2026-11-25: a PIX attribution artefact.)** The same shader
   dispatched twice with the same size and descriptors shows 0.2 µs once
   and 6,251 µs the other time: work queued earlier finishes at that event.
   Their translations are clean. The original note follows.
   **Some compute shaders are ~150 times slower per thread than others**,
   and at least one of them is trivial code:

   | compute shader | threads | time | per thread |
   |---|---|---|---|
   | `269b802f878c24e1` | 47,360 | 6.25 ms | ~130 ns |
   | `570f1473f4641b30` | 16,448 | 2.34 ms | ~140 ns |
   | `969c1cef8b783b39` | 3,328 | 0.63 ms | ~190 ns |
   | `2c3592e9bbad543f` | 921,600 | 0.73 ms | ~0.8 ns |
   | `a7eeed0646be1ab7` | 1,497,600 | 0.60 ms | ~0.4 ns |

   `570f1473f4641b30` translated is 179 lines of IR: zero stores into a few
   heap buffers, no loop. So the cost is where its buffers live, not its
   code. `shadow.rs` already says host copies in the PC's memory are read
   and written over the bus; in the reference scene 3.5 GB of "windows
   outside buffers" and every title buffer under 1 MiB are such copies
   (`card_limits` clamps the minimum to 1 MiB), and 5.6 GB of shared memory
   is in use. **Not yet shown directly**: which buffers these shaders touch.
5. **The title renders at 2560x1440 internally** although the display mode
   is 1920x1080. Shadow maps are 2048x8192 and take about 1,985 draws a
   frame.
6. **`--console series-s` loads once R9G9B9E5 render targets get a stand-in**
   (`VICTORY_RGB9E5_TARGET=1`, R11G11B10_FLOAT). It then runs ~8.6 fps with
   the card 71% busy, but the screen stays black: the title uses FSR4 there
   (`amd_fidelityfx_upscaler_xbox.dll`) and its 30 passes fail in
   `DeserializeComputePipelineStateX` (`E_NOTIMPL`), with no fallback.
7. **CPU side.** Lock convoys on global mutexes explained the idle CPU:
   `JOURNAL`, `GUARDS`, descriptor and in-place `VIEWS`, `BUFFERS`, `LOOSE`
   became `RwLock`s with read fast paths; `kept` and `RETIRED` are sharded;
   `HostHeap::mirror` has a lock-free check; clean guard blocks are also
   kept as bits read without the lock, and the guard lock is held only for
   the blocks that are not clean. `GetWriteWatch` on view memory is skipped
   (it was refused 25,000 times a frame). Submissions run on a thread of
   their own (`VICTORY_ASYNC_SUBMIT=1`). Preparing a frame is still
   ~75-90 ms, about half of it copying ~130 MB of title memory.

## Ruled out (measured)

- In-place title memory on a discrete card: card time 77 ms → 538-1,170 ms.
- Reading indirect arguments on the CPU: flicker (they are shader-written).
- Mesh shader maxima: all are already 128/128/128.
- `VICTORY_INDIRECT_CLAMP=0`: the title's own fatal path while the story
  loads (reported on fxceless/victory-fork PR #169).
- `VICTORY_INDIRECT_WINDOW=1`: no gain (and 50,000-96,000 groups asked
  instead of ~21,000).
- Runaway translated loops: `VICTORY_SHADER_LOOP_LIMIT=64` changes nothing.
- Group-sync barriers of wave64 run as two wave32: `VICTORY_GROUP_SYNC=0`
  gives no gain (slower).
- Removing the submitting thread's wait (`VICTORY_LATE_FILL_AHEAD=1` with
  the merge switches): the wait goes, the card goes to 95% busy, same fps.
- Copy-back alone: without it (`VICTORY_BARE=8`) 7.8-8.6 fps, card 98%.
- Windows in the card's memory (`VICTORY_CARD_WINDOWS_MB=4096`,
  `VICTORY_CARD_BUFFERS_MB=2048`): no gain; with 4.4 GB of card buffers
  the frame got worse (card wait 70 → 155 ms) because the card was at 10.9
  of an 11.5 GB budget. `VICTORY_TEXTURE_BUDGET_MB=3500` did not bring the
  host textures under 6.4 GB. So this one is untested rather than refuted.

## Card memory and levels on demand (branch `feature/levels-on-demand`)

Branch `feature/levels-on-demand` is `feature/watch-batch` with
fxceless/victory-fork `main` merged in (levels on demand, one host copy per
placed texture, the depth bounds test, card room kept for buffer copies)
and that repository's PR #795. Launch: `tools/perf/play-lod.ps1`.

Why the card's memory was full, from the code:

- Every texture is one committed resource of its whole mip chain, even when
  the title has only streamed in its small levels; placed textures got one
  host copy per address and description; nothing is aliased.
- Textures may fill the card up to 90% of the budget, card copies of
  buffers and windows only while it is under 80%: once textures filled it,
  buffers under 1 MiB, ~3.5 GB of windows and the indirect clamp's budget
  counter stayed in the PC's memory, read over the bus.
- At idle, other programs (OBS, Wallpaper Engine, browsers, Discord,
  launchers) held 4.3 GB of the 16 GB card: the budget was ~11.4 GB.

Measured, same opening-cutscene passage (~5,000 draws), RTX 5080:

| Build and settings | fps | card per frame | card busy | card memory in use |
|---|---|---|---|---|
| `feature/watch-batch` (old best) | 7.3 | ~58 ms | ~75% | - |
| merged, cache shared with older builds, levels on demand off | ~4.3 | ~135 ms | ~81% | 10.45 / 11.37 GB |
| merged, cache shared with older builds, levels on demand on | ~4.8 | ~105 ms | ~72% | 8.98 / 11.46 GB |
| merged, own cache, levels on demand on | 5.4-6.4 | 65-84 ms | 42-72% | 9.3 / 12.4 GB |

- Levels on demand leaves ~3 GB of the card unallocated (levels the title
  does not hold) and is worth ~10% by itself.
- **The merged build must not share its cache directory with older builds**
  (`victory-lod.toml`, `cache_dir` of its own): with the old pipeline
  library, runs after the first lost textures and lighting and took ~30 ms
  more of the card. With its own cache, textures and lighting are right
  (checked by eye) after the first launch translates everything again.
- The old best build is faster but draws less (no depth bounds test,
  missing textures): the numbers are not of the same picture.
- With the card under 75% busy the frame now follows the submitting
  thread: ~52% of its time waits for flights to land (`relay_wait`, the
  "title memory read" settles, ~57 ms a frame), ~17% refreshes host copies
  (`Shadow::update_free`, partly in contention on the guard lock).

Then, on the same branch and passage (build `victory-lod3.exe`, own cache,
levels on demand on):

| Added on top | fps | card busy | preparing |
|---|---|---|---|
| nothing | ~6.4 | ~68% | ~70 ms (+ ~57 ms waiting for flights) |
| `VICTORY_LATE_FILL_AHEAD=1`, `VICTORY_LATE_MERGE_RUNS=1000000`, `VICTORY_LATE_MERGE_GAP=64` | 6.2-6.6 | ~91% | ~110 ms, no wait |
| the same + `VICTORY_CARD_WINDOWS_MB=2048` | 7.4-8.1 | 52-59% | ~95 ms |
| the same with `VICTORY_CARD_WINDOWS_MB=4096`, `VICTORY_CARD_WINDOWS_PERCENT=95` | 6.8-7.0 | ~71% | ~120 ms |
| 2048 MB windows + `VICTORY_VIEW_GUARD=0` (experiment) | 6.9-8.6 | 73-81% | ~95 ms |

- Removing the wait alone moves the limit to the card; windows on the card
  (vertex, index and constant data read over the bus before) free it, and
  the frame then follows the submitting thread.
- With windows on the card the submitting thread is busy all the time:
  ~30% in `watch::guard` (lock contention on the guards: readers removed by
  `VICTORY_GUARD_KNOWN_BITS`, the writers remain), ~15-19%
  `merge_under_writes` (page by page, one thread), ~10-17% `sync_heaps`,
  ~15% texture refreshes.
- `VICTORY_VIEW_GUARD=0` turns off upstream's guard over the other views of
  a page; it may cost correctness (writes through another view missed).

## Later the same day (branch `feature/levels-on-demand`, build `victory-lod10.exe`)

- **`VICTORY_VIEW_GUARD=0` loses walls**: screenshots of the same shot show
  sky where the cell's walls are, with the guard on they are drawn. It
  gave ~+1 fps; it stays on.
- **Texture room**: `VICTORY_CARD_ROOM_FOR_TARGETS_MB=1024
  VICTORY_TEXTURE_ROOM_PERCENT=94` takes the uploads left without card
  memory from ~2,000-2,400 to 20-50 a run (fps unchanged). Most textures
  "waiting" wait for their title memory to be mapped, not for room.
- **Four agent-written patches** (commit c9ab142; switches in its message):
  preparing 109-118 -> 84-91 ms, copies 55 -> 29 ms, picture unchanged; fps
  6.6-7.2 -> 6.8-7.5 with the view guard on. A frame still takes ~140 ms with
  the card ~70% busy: the remaining limit is elsewhere (the title's
  recording threads, where each draw costs us 25-35 µs, are the suspect).
- `VICTORY_GUARD_PARK=1` (blocks found uncommitted parked, idea of
  fxceless PR #812): -15-20 ms of preparing but wrong lights; off.
- `VICTORY_GUARD_HOT` (hot blocks unguarded): 633 MB of copies a frame,
  3.8 fps, missing textures; off.
- Corgansm/victory-corgi 4102a03 on this machine with the same switches:
  7.6-8.5 fps, the same as this branch (its 9-13 fps reports are from
  other scenes or machines).
- Unattended runs: `VICTORY_INPUT_FILE` (keys from a file, `PadA` = pad A)
  now in release builds; scratch scripts press A at the start screen and
  take screenshots before closing the game.

### GPU-side switches from the agent patches (build `victory-lod11.exe`, view guard on)

| Switches on top of play-lod.ps1's earlier set | fps |
|---|---|
| none (card windows 2 GB) | ~7.1 |
| `VICTORY_CARD_BUFFERS_MIN_KB=512` | 7.0-7.7 |
| `VICTORY_UPLOAD_WINDOWS_MB=2048`, card windows 0 | 7.0-7.6 |
| **both** (two runs) | **8.2-8.9** |
| both, upload windows 4096 MB (+2048 MB room) | 6.0-7.8 (card full, 1,651 uploads refused) |
| both, `MIN_KB=64` | 6.1-7.3 (card 85-92% busy, more compares) |

The shader-visible heap list as a RwLock (commit 74eaa14) removed a wait of
about a quarter of the title's busiest threads (descriptor creation behind
the heap sync), with no fps change by itself.

### fxceless PRs #868 and #916, run-to-run spread (build `victory-lod14.exe`)

Back-to-back, same session and passage, play-lod.ps1's settings:
`victory-lod11.exe` 6.9-8.0 fps (the same build and settings gave 8.2-8.9
earlier: runs spread by about +-0.6 fps); `victory-lod14.exe` with
`VICTORY_SKIP_WC_COMPARE=1` (PR #868 by Chipsraine) 8.7-9.5 fps, copy-back
10-14 ms instead of 60-80, preparing 42-54 ms, card 80-90% busy: the card is
the limit again. PR #916 (only shaders that store by address mark title
memory written) is in that build, on by default. Picture checked right.

Also measured: the title resolves no timestamp queries in this passage and
its viewports stay 2560x1440 (frame line), so there is no dynamic
resolution to steer that way; mirroring the heap sync's slots on 4 threads
was slower (heap lock contention), now off by default.

### PIX capture of the current build (`victory-lod14`, diagnostics, 2026-11-25 evening)

`G:\GTA VI\profiles\events3.csv` (local; `pixagg.py events3.csv 40`). One
frame of the opening cutscene: **92 ms on the card** (117 ms in the morning
capture). By kind: indirect compute 37.7 ms (1,320), amplified mesh 22.3 ms
(3,662), direct dispatch 8.2 ms, our own work ~13 ms (texture copies 4.9,
barriers 4.0, clamps and compares 2.4, buffer copies 1.6).

The biggest single items are indirect compute dispatches of a few title
shaders: 559d9f0184e15fe3 9.0 ms, 715da27a0501263f 5.4 ms, b96880482a17f632
4.4 ms, 36f04a4ed0ad1d9b 2.3 ms (559d9f took 8.7 ms in the morning capture
too). PIX reports no invocations for any ExecuteIndirect (0 of 1,320, while
5,038 of 5,867 direct dispatches have some), so "zero threads" there is a
limit of the counter, not an empty dispatch: these are real work, and the
lever is how they are translated (or how much they are asked to do).

### Translator switches (build `victory-lod14.exe`, play-lod.ps1's settings)

Each translation variant has a shader-cache key and pipelines of its own:
switching needs a warm-up run before measuring (a run that rebuilds
pipelines reads ~2 fps with the card ~17% busy).

| Variant | fps |
|---|---|
| default (control, measured last) | 9.5-10.1 (card 90-96% busy) |
| `VICTORY_LEGACY_MUL=2 VICTORY_SMEM_WIDE=1` | 9.1-10.0 |
| `VICTORY_LEGACY_MUL=1 VICTORY_SMEM_WIDE=1` | 7.7-9.5 |

No gain from either: they stay off. The frame is now card-bound at ~100 ms.
A run of the same branch and settings on an RTX 4090 + i9-13900K (a
friend's, first launch): 14.4 fps, 69.6 ms a frame, preparing 30.8 ms, card
84-92% busy, budget 23.4 GB; the card time ratio to the 5080 (~1.46) is the
ratio of their shader throughput (~1.47): the title's shader work bounds the
frame on both. Both machines drop ~350 draws a frame: 8 pipelines refused
with E_INVALIDARG (7 compute, 1 mesh) and one compute program not translated
("lane mask exchanged where not every lane runs").

### Refused pipelines fixed by fxceless PR #854 (build `victory-lod15.exe`)

The D3D12 debug layer (`VICTORY_D3D12_DEBUG=1`, diagnostics build) gives the
reason for the 7 refused compute pipelines: "Shader wants wave lane count of
64 but maximum wave lane count for device is 32" (NVIDIA). PR #854
(PiloteCode, wave64 programs in a paired form on 32-lane GPUs, mesh programs
over 32 KiB linked with their pixel program) is merged: refused pipelines
8 -> 1 (mesh f5e50eb2108c425b + pixel ce3f0432a08f1d35, 2 draws a frame),
fps unchanged (8.3-10.0), and the cell window's frosted glass is drawn now
(checked right against the game). Most of the ~300-990 draws a frame still
"dropped without a host pipeline" are the ray-tracing structure builder's,
which is off in this branch; one compute program (f133e35efed2a48a, about one
dispatch a frame) does not translate.

Also merged: dkdknight/victory-fork PR #1 (tibzejoker), `VICTORY_LIVE_FILE`:
`bare=<bits>` and `skip=<hashes>` changed while the title runs. Its numbers
on an RTX 4090 (diagnostics build): dispatches off (`bare=2`) 6.3 -> 11.1 fps,
the title's compute work being the largest part of the card's frame.

### Nsight Graphics GPU Trace (RTX 5080, `victory-lod15`, opening cutscene)

`tools/ngfx-trace.ps1` (F11 in the game window; do not pass `--platform`
to ngfx: Qt takes it as its own option and fails to start). Three frames,
"Top-Level Triage" metrics (`G:\GTA VI\profiles
gfx\BASE\GPUTRACE_FRAME.xls`):

- shader units (SM) 5-8% of peak, DRAM 5-7%, L2 3-4.5%; load latency
  ~1,200 cycles, texture ~1,100;
- front end busy: graphics 21-38%, compute 23-43% of the frame;
- 4,900-6,100 "go idle" (wait for idle) a frame, but the cycles stalled on
  them are 9-16 M at ~2 GHz: **5-8 ms a frame**, not the bulk;
- VRAM bytes demoted 0.

So the card is mostly not working: it waits for work (submissions, value
waits) and, when it works, for memory. Barrier experiments agree:
`VICTORY_BARRIER_TEST=indirect` (diagnostics) 7.7 -> 8.9 fps; in release
`VICTORY_CLAMP_WAIT=never` (new: no UAV barrier around the clamp, the
title's own barrier and the scratch transitions order it) and
`VICTORY_INDIRECT_LAZY=1` (new: scratch left in INDIRECT_ARGUMENT after an
execution) show no difference beyond the run-to-run spread (about +-1 fps
this evening). Both are off by default. tibzejoker measured the same on an
RTX 4090 (removing the waits: no change). Removing the title's barrier
groups (`groups`) crashed once; `both` 9.4 fps with a wrong picture.

### A new cache shows a darker cell for several runs (2026-11-25)

Comparing builds with a cache of their own each (to keep one build's
pipelines from another's) showed the cell's walls dark, the poster missing
and bigger blocky patches at the bottom, for **every** build, the reference
`victory-lod15.exe` included, for at least four runs. With the long-used
cache (`victory-lod-data`: ~23,000 shader files from many builds, 2,849
pipelines in `early-pipelines.txt` against ~2,100 for a new one) the same
builds draw the lit walls. Translations are keyed by the translator's
fingerprint, so a copy of an old cache does not hand a build another
translator's output: it brings the pipeline library and the early list.
The lighting of the cell also varies from run to run on the same build and
cache (warm-up, when the textures and the lighting passes arrive), so a
picture has to be compared over several runs. Testers starting from an
empty cache should expect a darker opening for the first runs.

With a copy of the old cache, PRs #2 and #3 (tibzejoker: fxceless main,
every pipeline of the scene made) measured 8.0-9.3 fps with
`BACK_OWN_VIEW=1 COPY_PLAIN=1 FAST_CORES=1 ASYNC_SUBMIT=2 JOIN_PARTS=1`
against 8.6-9.9 for lod15 just after, the picture as good (walls lit,
poster drawn). No gain on the 5080 beyond the spread; merged for the
pipelines and for the gain measured on the RTX 4090 (12.8 -> 13.8 fps).

## Open questions

- Which buffers the slow compute shaders read and write, and whether
  putting exactly those in the card's memory removes the cost.
- The guards' write lock: `VirtualProtect` calls made under it, one per
  block for the other views of a page.
- FSR4's precompiled pipelines for the Series S mode.

## Tools (`tools/perf/`)

- `sampler.py <image> <pdb dir> [secs] [ms] [filter|0] [chain names...]`:
  stack sampler by suspend + `StackWalk64`, no admin rights. Filter
  `Host::submit` for the submitting thread, `relay_worker` for the landing
  thread; chain names such as `lock_contended` print who waits on what.
- `joinlog.py in out`, `auto5.sh <log> <exe>`: wait for the reference
  passage and print frame lines, card occupation and settles.
- **PIX capture that completes**: `VICTORY_PIX=1`,
  `VICTORY_PIX_TRIGGER=<file>`, `VICTORY_PARK_NULL_WRITE=1` (the title's
  watchdog fires its fatal null write ~36 s into a capture; this parks that
  thread), and in a diagnostics build `VICTORY_PIX_MARKERS=1` to name every
  command. Pipelines are remade under the capturer for about six minutes.
  `pixwait4.sh` triggers when the scene is whole. Analysis needs Windows
  developer mode:
  `MSYS_NO_PATHCONV=1 pixtool open-capture X.wpix save-event-list e.csv "--counters=*Duration*" "--counters=*Invocations"`,
  then `pixagg.py e.csv 45`.
- `VICTORY_SHADER_SAVE=<dir>` saves each used shader as given and as
  translated (also on cache hits); `dxc -dumpbin` reads the `.dxil`. Keep
  these local: they are game data.
- `VICTORY_SHADER_HOT_DIR=<dir>` swaps a shader's code while the title
  runs: every half second `<dir>/<hash>.dxil` (16 lowercase hex digits, as
  logged) is looked at, and when it appears or changes every host pipeline
  with that shader is made again with it (same root signature, other stages
  and state); removed, they are made again from the translation. Log line
  `hot shader <hash>: <n> pipelines made again`. List hashes in
  `<dir>/want.txt` to get their translation in `<dir>/orig/<hash>.dxil` once
  used; edit (`dxc -dumpbin`, change the HLSL or IR, recompile with `dxc`
  keeping the bindings) and drop the result in `<dir>`. Such pipelines stay
  out of the pipeline library; a refused one keeps the old pipeline (WARN
  with the HRESULT). Old pipelines are never released, so keep the edits
  to a few dozen per run.
- `VICTORY_BARE=<bits>` (1 mesh draws, 2 compute, 4 refreshes, 8 copy-back,
  16 other draws, 32 texture uploads, 64 fills and copies) and
  `VICTORY_MESH_TEST=skip|scissor|noexec` for isolation runs.
- `VICTORY_SHADER_CACHE_KEY=<name>` shares one shader cache between builds
  that leave the translator alone (`amp-2026-11-25` for builds with the
  amplification stage).
