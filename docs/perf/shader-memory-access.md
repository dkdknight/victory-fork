# Memory accesses of translated shaders (`VICTORY_SHADER_FAST_MEM`)

Branch `shader/memory-access` (from `feature/levels-on-demand`), plan item
3b: how the costliest shaders' memory accesses are translated. Written with
the help of an AI assistant (Claude Code). Not measured in a game run yet:
the owner runs one global test at the end. No game data is in this
repository; shader hashes are only the names the logs print.

## Why

Nsight on the RTX 5080 (opening cutscene): the shader units are 5-8% busy,
a global load waits ~1,200 cycles, DRAM is 5-7% busy, and 250-430 MB a
frame are **written** over PCIe. The PIX capture's costliest dispatches are
indirect compute shaders `559d9f0184e15fe3` (9.0 ms), `715da27a0501263f`
(5.4 ms), `b96880482a17f632` (4.4 ms) and `36f04a4ed0ad1d9b` (2.3 ms).

## What the four shaders do, and what their translation costs

Translated with `victory translate --list --validate` on their containers
(`VICTORY_EMIT_DUMP=<dir>` writes the DXIL, `dxc -dumpbin` reads it).

| shader | console code | translation |
|---|---|---|
| `559d9f0184e15fe3` | a ray walk: 10 `image_load`, a stack in LDS, a loop with 2 `image_bvh_intersect_ray`, 6 `global_load_dword`, 19 `s_buffer_load_dword` | 15 accesses to title memory by address (the node tests and the global loads), **each one a directory look, a store of a fault note into a buffer in the PC's memory by every lane, and a load through a non-uniform view**; the node test is inside the walk's loop, so that store runs once per node per wave |
| `715da27a0501263f` | binning: 23 descriptor loads, 15 `buffer_store_format_xyzw`, 6 `buffer_load_format_x`, 5 `buffer_atomic_add`, one `buffer_load_dwordx3` **per lane on a constant buffer** | clean apart from the per-lane constant buffer read: 3 `cbufferLoadLegacy` with a different row per lane (NVIDIA replays a constant read once per distinct address in a warp), each followed by a choice among the row's 4 dwords |
| `b96880482a17f632` | waterfall loops over per-lane descriptors (`s_load_dwordx4 s[0:3], s[6:7], s0` + `s_cbranch_execnz`), `buffer_load_dword..x4` | clean: one non-uniform handle per waterfall descriptor, loads at the console's width (x4 stays one `rawBufferLoad` of mask 15). Its cost is the latency of where its buffers are, not the translation |
| `36f04a4ed0ad1d9b` | a ray walk over instances: 4 `image_bvh64_intersect_ray`, 91 `global_load_dword` + x2/x3/x4; per node ten loads at offsets 64..108 of **one address** | 111 accesses by address, each with its own directory look and its own fault-note store by every lane |

The overheads the plan asked about, checked in these four and counted over
the 2,497 programs saved in `G:\GTA VI\shader-analysis\all` (translations
of the morning's build):

- **Fault notes of title memory by address (the big one).** `Lower::memory`
  stored, for every lane of every access, a dword into the faults buffer
  (`SLOT_FAULTS`): the page number where the page had no copy, else the
  sink at dword 0. That buffer is a `shared_buffer` (custom heap, pool L0:
  the PC's memory), so each wave's store of each access crossed the bus.
  14 programs have such accesses (1,372 sites), the two ray walks above
  among them.
- **Directory look per access.** One look per access even for ten loads of
  one address; and each look precedes the data load (two dependent
  latencies).
- **Strides table per access.** A buffer picked from the view heap reads
  its element stride from the host's table (`SLOT_STRIDES`) once per
  *block* that reads it: of 4,537 such lookups in 480 programs, 3,584
  repeat the index of another lookup of the same program.
- **Per-lane constant buffer reads.** Each dword of a
  `buffer_load_dwordxN` on a CBV was its own `cbufferLoadLegacy` and a
  4-way choice.
- **`s_buffer_load` turned into per-lane loads**: no. Scalar constant
  buffer loads become `cbufferLoadLegacy` at constant rows; scalar loads of
  buffer views are split into one-dword raw loads (`VICTORY_SMEM_WIDE=1`
  reads up to four at a time; measured without gain earlier, left apart).
- **32-bit loads where the console loads dwordx4**: no; vector loads keep
  their width (14,639 x4, 8,637 x3, 11,578 x2 of 424k raw loads; the
  one-dword ones are mostly the scalar loads above and the console's own).
- **Descriptor fetches per access**: handles are made once per DXIL block
  and the heap index arithmetic is shared by our clean-up passes; nothing
  to gain.
- **Bounds checks**: only `UMin` clamps of heap indices (ALU, cheap).
- **Typed loads where raw would do**: typed loads are only the console's
  format loads (15,725 through SRVs, 386 through UAVs).
- **UAV loads where SRVs would do**: 2,738 raw loads of 424k go through
  `RWByteAddressBuffer`: nearly all are the title-memory copies' views
  (written by the same programs) and buffers the title itself binds as
  UAVs. An SRV twin of those views needs a second view per copy and buffer
  states the host does not keep; not done.

## What changed (all behind `VICTORY_SHADER_FAST_MEM=1`, off by default)

The switch is read by the translator, so it is part of the shader cache key
(`translation_fingerprint` collects every `"VICTORY_*"` string of the
translator's source): **a run with it needs its own bake (warm-up) run**.
Every change is exact (same bytes read and written, same faults noted).

1. **Fault notes behind a branch** (`Lower::memory_note`): only lanes whose
   page has no copy store, in a branch; the sink is never written. No
   store over the bus any more on the common path.
2. **Shared directory looks** (`Lower::memory_at`): accesses through the
   same address dwords share one look in IR blocks it dominates. An access
   whose byte (low 16 bits of the address plus its offset) stays on the
   look's page takes the look's copy; one that leaves it (each lane checks,
   unsigned compare) looks again in a branch. Each access still notes its
   own page. `global_load_*`, `global_store_*`, `global_atomic_*` and
   scalar loads by address use it.
3. **The second half of a 32-bit box node** (`intersect.rs`): looked for
   before the node's kind is tested, through the shared look (a new look
   only when the node ends its page); its fault note is for lanes whose
   node is a 32-bit box. Before, it was a look of its own inside the box
   branch.
4. **Per-lane constant buffer rows read once** (`Lower::buffer`, CBV
   case): with an index and a fixed byte offset, each 16-byte row the
   dwords lie in is one `cbufferLoadLegacy` at `index + byte / 16`, and
   each dword a fixed component of it.
5. **A heap buffer's stride read once per descriptor**
   (`Lower::slot_stride`): read in the DXIL block the descriptor was loaded
   in (recorded per scalar load), which every reader comes through; not in
   the paired wave64 form, where each console lane keeps its own.

## Log lines

- Once, at info, when the switch is on (`victory_gpu::shader_cache`):
  `VICTORY_SHADER_FAST_MEM=1: translated memory accesses with less overhead (fault notes of title memory by address behind a branch, shared directory looks, constant buffer rows read once per lane access, a heap buffer's stride read once per descriptor); translations of their own in the shader cache`
- Per translated program (cache misses, so the bake run), the
  `shader translated` line gets `fast_mem_notes`, `fast_mem_shared_looks`,
  `fast_mem_cbv_dwords`, `fast_mem_strides` when the switch changed it,
  e.g. for `36f04a4ed0ad1d9b`: `fast_mem_notes=111 fast_mem_shared_looks=36 fast_mem_cbv_dwords=0 fast_mem_strides=0`
  (`559d9f0184e15fe3`: 15 and 6; `715da27a0501263f`: 2 CBV dwords).
- Every 5 s, when any program was changed this run:
  `translated with VICTORY_SHADER_FAST_MEM=1 since the start (shader cache misses only): N programs changed; ... fault notes behind a branch (... with a shared directory look), ... CBV dwords from a row read once, ... stride lookups from the descriptor's`
- **Per frame (the proof in the real run, cache or not)**, `victory_gpu::memory`:
  `title memory by address per frame: 14.0 dispatches and draws, 0.0 pages found without a copy; the fault notes' sink written in 0 of 10 frames (each such frame, every lane of every access stored there over the bus; none with VICTORY_SHADER_FAST_MEM=1)`
  Without the switch the sink count equals the frame count whenever a
  shader reads title memory by address; with it, it must be 0 (a non-zero
  count means some translation came from a build without it).
- Device removed: the context line (`when the host device went: ...`)
  says `title memory by address: VICTORY_SHADER_FAST_MEM on|off, N
  dispatches and draws made ready since the last frame line, M frames of
  them with the faults' sink written`.
- `victory translate --list` prints per program what the switch changed
  (`fast memory: ...`), for offline checks.

## Tests

- `emit::memory_tests` (host GPU and WARP, each skipped without a device;
  DXIL validated with `dxil.dll`): reads and writes by address around a
  page boundary (offsets 0, 4, 64, -4 and a store at 512) with both pages
  present and with one missing, with and without the switch: same values,
  same fault notes, the sink untouched with it and written without; a
  per-lane CBV read across rows (x4 at 8, x3 at 4); a heap-picked buffer
  read in two blocks with its stride from the table.
- `emit::intersect_tests`: the node tests over title memory (stand-in)
  match the CPU reference with the switch; through the directory they
  lower and validate with one shared look.
- All 2,502 saved containers (1,001 vertex and pixel, 1,089 compute, 412
  mesh programs) translate and validate with the switch as without; 705
  programs change (1,372 fault notes behind a branch, 430 of them with a
  shared look, 858 CBV dwords from a row read once, 14,531 stride lookups
  served by the descriptor's). The host pipelines of the four shaders are
  made as without the switch (`b96880482a17f632` is refused by the offline
  tool's root signature with E_INVALIDARG in both modes).

## Risks

- A new bake: the switch changes the cache key, so the first run with it
  translates everything again (the "cuisson" run).
- More blocks: each access by address gets one branch (the note) and one
  more when it has an offset (the page check). They are rarely taken, but
  `36f04a4ed0ad1d9b` goes from 502 to 804 DXIL branches; the driver's
  compiler has more to schedule.
- The shared look relies on IR dominance carrying over to the DXIL blocks
  (as every other value of the lowering does); a mistake there would show
  as a DXIL validation failure, and none appeared.
- The stride hoist puts the lookup in the descriptor's block, where it runs
  for every lane even if the buffer is read under a branch (the index is
  clamped to the table, so the read is always in bounds).
- Gain unknown: it removes per-access stores over the bus and dependent
  loads in the two ray walks and the culling programs that read heap
  buffers, but most of the frame's programs do not change.

## Review (second pass, branch `shader-memory-access`)

The review fixes are on `shader-memory-access`, made from the tip of
`shader/memory-access` (`e89f175`). That branch was checked out in the
first agent's worktree, and this worktree could not take it over. Merge
`shader-memory-access`: it has both.

Checked and found right:

- **Exactness.** Fault notes: the same pages are noted at the same places.
  Only the sink (dword 0) is no longer written. Shared looks: an access
  stays on the look's page exactly when `(address & 0xFFFF) + offset` is
  in `0..0x10000` as an unsigned compare. Negative offsets wrap to large
  numbers and look again. A carry into the high dword needs the page to
  be left, so the high dword never needs its own check. The directory
  index is still clamped, so lanes that do not run and wild pointers stay
  inside it. CBV rows: `(16 i + b) >> 4` and `i + b / 16` differ only in
  the top 4 bits, for indexes of 2^28 rows and more. Both are far past any
  constant buffer (64 KiB).
- **Blocks.** Every caller of `memory_at` takes the block to go on in
  from `self.cur` after it (loads, stores, atomics, scalar loads, the
  node test). The atomic `sub` negation stays in the block before the
  branch, which dominates its use. Looks are never recorded inside a
  branch (the page check's second look is not kept). No memory access is
  lowered inside another instruction's guarded region, so a look's block
  dominates the rest of its IR block. The loop limit's exits go to a block
  of their own that returns, so IR dominance carries over to DXIL.
- **GPU safety.** No loop, fence or barrier is added. The branches only
  hold a store to the faults buffer or a directory read. The faults
  buffer is a `shared_buffer` (custom heap, write-back CPU pages, pool L0),
  so the host's new read of the sink is not a read of write-combined
  memory. The device-removed note takes no lock (atomics only).
- **Fingerprint.** `"VICTORY_SHADER_FAST_MEM"` is a string literal in the
  translator's source, so `build.rs` lists it and its value joins the
  cache key. Any set value changes the key, `=0` too: that translates as
  off but bakes again.

Fixed:

1. **The stride hoist had no dominance check** (`Lower::slot_stride`). It
   trusted the descriptor's block. SSA makes the load dominate its readers
   today. But a value used where it is not defined fails DXIL validation
   and loses the pipeline (a missing draw or dispatch). So the hoist now
   also needs the load's IR block to dominate the reader's. Over the 2,502
   saved programs it refuses nothing (14,531 hoisted lookups, as before).
2. **No test covered a look across blocks.** New test
   `a_look_is_shared_only_from_a_block_that_dominates_the_access`. A look
   made in a branch is not used at the join: the join looks again, and
   its second access shares the join's look. It runs on the GPU and on
   WARP, with and without the switch, and checks results and notes. With
   the dominance check forced to always say yes, the test fails DXIL
   validation.
3. **The switch's info line depended on the shader cache.** It was said
   only where the cache key is worked out. Now it is said on the first
   translation either way (`shader_cache::fast_mem_said`).
4. **Per-program counts could land on the wrong hash.** The thread-local
   count was only taken after a translation. A count left by a lowering
   that failed later, or by another path on the thread, was printed on the
   next cache hit's `shader translated` line. Now it is cleared before each
   translation (`pipeline.rs`, `victory translate --list`).
5. **The new log strings had runs of 10-26 spaces in the middle** (line
   breaks inside the literals without a backslash). Fixed.

Left as they are (minor, logs only):

- In the paired wave64 form, each access is lowered twice, so its sites
  count twice in the `fast_mem_*` counts.
- `FAST_MEM_TOTALS` counts a program that lowered but then failed a later
  step.

Gain estimate: the two ray walks are ~11 ms of a ~92 ms frame. Removing
their per-access store over the bus and some of their dependent directory
reads might save 1-3 ms there. The hoisted stride reads help the culling
and mesh programs a little. About +0.2 to +1 fps, not measured. This change
alone does not come close to 15 fps.
