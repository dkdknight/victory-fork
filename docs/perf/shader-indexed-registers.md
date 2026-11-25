# Indexed registers in the translator (plan item 3a)

Branch `shader/indexed-registers`, from `feature/levels-on-demand`.

## What the plan assumed, and what the translator does

The plan said: the console shaders that index their registers (`v_movrel*`)
become arrays on our side, which NVIDIA's driver puts in local memory.

The translator has **no such arrays**. It writes no `alloca` and no array
of its own registers anywhere in `crates/victory-shader/src/emit/`:

| Construct | Where | How it is lowered |
|---|---|---|
| `v_movrels_b32`, `v_movreld_b32`, `v_movrelsd_b32` (M0-relative VGPRs) | `lower.rs`, `"v_movrels_b32" \| ...` | Already a compare-and-select chain on M0: one compare and one select per register from the base up to the program's VGPR count. No array. |
| `v_movrelsd_2_b32`, `v_swaprel_b32` | `ir.rs` knows them, `lower.rs` does not | Refused (none in the title's run set). |
| `s_movrel*` | `ir.rs` | Refused at IR build (none in the title's run set). |
| `scratch_*`, `s_scratch_*` (console private memory) | decoded only | Refused (none in the title's run set). |
| The program's code table (constants the console compiler appends after `s_endpgm`, read through `s_getpc_b64`) | `lower.rs`, `code_dword` | An internal constant array (`@code = internal constant [N x i32]`), indexed with a computed index. **The only indexable array the translator writes.** |
| LDS | `shader.rs`, `lds()` | Group-shared memory (address space 3), not per-thread: not local memory. |
| Root CBV blob, user data | `shader.rs` | Constant buffers, not per-thread arrays. |

GFX10 has no `s_set_gpr_idx` (that is GFX8/9).

## How many of the title's shaders hit each

Census of the 2,502 shaders the opening cutscene runs (the containers in
`G:\GTA VI\shader-analysis\all`, as the title has them), with
`VICTORY_SHADER_CENSUS` and the new columns (see "Log lines"):

| | programs | instructions |
|---|---|---|
| `v_movrel*` | 20 | 228 console instructions, **20,057** host compares and selects (`indexed_select`) |
| code-table reads | 150 | 1,364 loads from the constant array, all at a computed index; 521 of them per lane (vector loads, 62 programs), the rest scalar |
| code-table sizes | 150 | none of 32 dwords or less; 31 programs 33-64; 107 programs 65-256; 12 larger (a table runs from the first byte read to the end of the code) |

Over the whole shader pack (`shaders.rpf`, 26,503 containers, 26,084
programs, `--jobs 8`): 37 programs use `v_movrel*` (398 console
instructions, 37,501 host compares and selects); 665 read a code table
(7,838 loads, 4,876 of them per lane); table sizes: 102 programs 33-64
dwords, 510 programs 65-256, 53 larger, none of 32 or less.

M0 for the relative accesses comes from `v_readfirstlane_b32` (8 programs:
the console compiler's loop over the lanes' indices), from shifts and ors
(4), or from loop counters (phi, `s_add_i32`): never a constant, so no range
can be proved from it.

Of the four costly shaders of the PIX capture, **none** uses `v_movrel*` or a
code table: 559d9f0184e15fe3, 715da27a0501263f, b96880482a17f632 and
36f04a4ed0ad1d9b are unchanged by this branch (same module with the switch
on or off).

Weighted by the PIX capture of one frame (92 ms, `events3.csv`): the draws
and dispatches whose shaders use `v_movrel*` take 1.29 ms; those whose
shaders read a code table take 4.46 ms; the ones the switch changes
(`VICTORY_SHADER_NO_ARRAYS=1`) take **1.24 ms** in all. This item cannot give
the frame much: at best a part of those 1.24 ms.

### What Nsight says about local memory

The Nsight export (`GPUTRACE_FRAME.xls`) does count local memory traffic:
`l1tex__t_sectors_pipe_lsu_mem_local_*` ~3.4e8 sectors read and ~2.7e8
written, against ~2.2e9 global sectors read and ~1.1e8 written (same units,
same range). Local stores are about 2.4 times the global stores. Since the
translator writes no per-thread array, that local traffic is the driver's:
either register spills of large programs, or the code tables, if the driver
copies an internal constant array into local memory instead of a constant
bank. `VICTORY_SHADER_NO_ARRAYS=2 VICTORY_SHADER_NO_ARRAYS_MAX=256` removes
the tables of 138 of the 150 programs: an Nsight trace with and without it
tells which (if the local sectors do not move, they are spills, and the
next lead is register pressure: plan item 3b).

## The change: `VICTORY_SHADER_NO_ARRAYS` (translator switch, off by default)

Indexed reads stay in registers, as a **tree of selects on the index's
bits** (`select_tree` in `lower.rs`):

- **M0-relative reads** (`v_movrels_b32`, `v_movrelsd_b32`): n registers in
  range give n - 1 selects and log2(n) bit tests, plus one range check (0
  past the range, as the chain gave), instead of n compares and n selects.
  The relative writes keep their selects (one per register: each register
  needs its own value).
- **Code tables** of at most `VICTORY_SHADER_NO_ARRAYS_MAX` dwords (default
  64): the clamped index picks among the table's constants by a select tree;
  no array is indexed. With a constant index, the clean-up passes fold the
  tree to the constant.

Values:

| Switch | Value | Effect |
|---|---|---|
| `VICTORY_SHADER_NO_ARRAYS` | unset, `0` | off: the translation is unchanged |
| | `1` | M0-relative reads as trees; code tables read **per lane** (vector loads, an index per lane) as trees |
| | `2` | as `1`, and code tables read by **scalar loads** too (one index for the wave, which a constant array serves well: measure before keeping it) |
| `VICTORY_SHADER_NO_ARRAYS_MAX` | 2-256, default 64 | the largest table kept in registers; larger ones stay arrays |

Both are translator switches: their values are in the translation
fingerprint (`translation_fingerprint`, from the switch list `build.rs`
collects), so the shader cache keeps the translations of each setting
apart. Changing either needs a new bake (warm-up) of the shader cache.
They are not live switches (a shader is translated once).

Effect on the 2,502 shaders of the cutscene, all validated by `dxil.dll`:

| | off | `=1` | `=2`, MAX=64 | `=1`, MAX=256 |
|---|---|---|---|---|
| host instructions | 19,077,853 | 19,070,718 | 19,074,445 | 19,100,814 |
| M0-relative compares and selects | 20,057 | 10,802 | 10,802 | 10,802 |
| code-table loads left (per lane) | 1,364 (521) | 1,317 (474) | 1,172 (474) | 1,006 (163) |
| code-table selects | 0 | 678 | 2,955 | 26,794 |
| programs changed | | 28 | | |

Whole pack with `=1`: relative compares and selects 37,501 -> 19,049;
code-table loads 7,838 -> 7,315 (43 programs read their table as a tree,
9,255 selects); every program that emitted validates as with the switch off
(71 mesh programs over the 32 KiB output limit fail validation alone in
both settings: the runtime links those with their pixel shader).

The widest relative program, ba2ea928faffd2b0 (68 `v_movrel*`, 112
registers in range), goes from 9,587 to 7,693 host instructions
(4e296b1885c37b29: 8,446 to 6,177; 95988cc1e16f1d89: 6,438 to 4,870).

## Fix: the translator's build script read another checkout (pure bug fix, no switch)

`crates/victory-shader/build.rs` took its package folder from
`env!("CARGO_MANIFEST_DIR")`, the folder when the script was **compiled**.
A target folder copied from another checkout (as every agent branch seeds
`target-main`) keeps that checkout's compiled script, and cargo does not
recompile it: the script then hashed the **other checkout's** translator
source and listed **its** switches. Measured here: the script in the seeded
target read `G:\GTA VI\victory-integ\crates\victory-shader\src` (another one
read `G:\GTA VI\wt-22d8455\...`), and the new switch was missing from the
fingerprint (the unit test `the_no_arrays_switches_are_in_the_fingerprint`
caught it). Consequences before the fix:

- a build of another branch in such a target computed the shader cache key
  from the wrong source: translator changes did not change the key, and the
  cache could hand out translations of the other checkout's translator;
- switches only this branch has were not part of the key.

The script now reads `CARGO_MANIFEST_DIR` from its environment at run time
(cargo sets it for every run). **Every translator branch built in a copied
target folder before this fix may have run with stale cached shaders**;
bake the cache again after merging.

## Log lines

- Once at start, when the switch is on (`victory-gpu`, `shader_cache.rs`):
  `VICTORY_SHADER_NO_ARRAYS: indexed reads as select trees (...)` with
  `relative`, `lane_tables`, `scalar_tables`.
- Each frame from frame 256, on the existing line, when the switch is on:
  `shaders from the cache and translated so far (no_arrays: ...)` with
  `hits misses no_arrays_tables no_arrays_relative no_arrays_saved`: code-table
  and M0-relative reads this run translated as trees, and the compare/select
  instructions the trees saved over the chains (shaders from the cache are
  not counted: they were translated by the bake).
- `victory translate` and `victory translate --warm` end with
  `no arrays    VICTORY_SHADER_NO_ARRAYS NoArrays { ... }: <n> code-table reads and <m> M0-relative reads as select trees, ~<k> instructions saved over the chains`.
- Device removed (`when the host device went: ...`): says the switch's
  setting and the counts, when it is on.
- `VICTORY_SHADER_CENSUS=<file>` with `victory translate`: new columns
  `indexed_select`, `table_load`, `table_dynamic`, `table_lane`,
  `table_select`, `table_dwords` (host module) and `c_movrel`,
  `c_movrel_span`, `c_m0_defs` (console program: relative accesses, the
  widest range, what sets M0).

Example (`victory translate --validate --jobs 8 shaders.rpf` with `=1`):

```
no arrays    VICTORY_SHADER_NO_ARRAYS NoArrays { relative: true, lane_tables: 64, scalar_tables: 0 }: 533 code-table reads and 318 M0-relative reads as select trees, ~16390 instructions saved over the chains
```

In a run, the per-frame line then reads like
`hits=2480 misses=22 no_arrays_tables=0 no_arrays_relative=3 no_arrays_saved=210 shaders from the cache and translated so far (...)`
(illustrative values: after a bake, almost everything comes from the cache).

## Tests

- `emit::tests::the_no_arrays_switch_parses_or_stays_off`, and
  `the_no_arrays_switches_are_in_the_fingerprint` (both names are in the
  switch list the cache key reads).
- `emit::valu_tests::relative_register_reads_are_the_same_as_a_tree`:
  `v_movrels_b32`, `v_movreld_b32` then `v_movrels_b32`, `v_movrelsd_b32`
  over 35 registers (not a power of two), M0 from 0 to 43, 255,
  0xFFFFFFFF: the same result in every form, the expected register in range;
  run on the hardware GPU and on WARP; checks the module did change.
- `emit::valu_tests::code_tables_read_the_same_as_a_tree`: a 20-dword table
  after `s_endpgm`, read per lane (`global_load_dword`) and by a scalar load
  at indices 0-23, 255 and wrapping byte offsets: the same result in every
  form, the table's dword (its last past the end), on the hardware GPU and
  WARP; checks the module changes only in the forms that should change it.
- All 2,502 shaders of the cutscene translate and validate with `dxil.dll`
  in each setting above.

## Risks

- The gain is small by construction: the shaders it changes take ~1.2 ms of
  a 92 ms frame. Do not expect fps from it alone.
- Mode `2` turns the uniform (scalar) code-table reads into select trees,
  where a constant array may be as fast or faster: measure.
- A large `VICTORY_SHADER_NO_ARRAYS_MAX` makes long trees (up to 255 selects
  per read) and larger programs; the census shows +23 k instructions for
  `=1` at 256.
- The switch changes translations: the shader cache must be baked again with
  the same setting, or the run translates at first use (stutters).
