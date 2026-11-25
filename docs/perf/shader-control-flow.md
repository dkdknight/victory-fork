# Control flow in the translation of the costliest shaders (`shader/control-flow`)

Plan item 3b, part two: how EXEC masks, waterfall loops, wave64 programs on 32-lane hosts,
lane-crossing operations, added group barriers, loop limits and per-lane scalar code are
translated in the programs that cost the most card time, and what changed. The other part of
3b (memory-access lowering) is another branch. Branch `shader/control-flow`, from
`feature/levels-on-demand`. Written with the help of an AI assistant (Claude Code). Nothing
here was measured in the game: the owner's global test does that.

## Method

- `victory shader-info --disasm` on the console containers (`G:\GTA VI\shader-analysis\all`,
  local game data), `victory translate --list` with `VICTORY_EMIT_DUMP` and
  `VICTORY_HOST_MAX_WAVE=32` (the RTX 5080's waves), `dxc -dumpbin` on our DXIL.
- The four indirect compute programs of the PIX capture (559d9f0184e15fe3 9.0 ms,
  715da27a0501263f 5.4 ms, b96880482a17f632 4.4 ms, 36f04a4ed0ad1d9b 2.3 ms), the next
  twelve compute programs of the same capture, and the eleven costliest amplified mesh
  programs.
- Then every title program saved there (2,502: 1,001 vertex and pixel, 1,089 compute, 412
  mesh), translated and validated by `dxil.dll` with and without the switch.

## What each costly program suffers from

| program | card time (PIX) | form | what the translation costs it | with `VICTORY_SHADER_FAST_FLOW=1` |
|---|---|---|---|---|
| 559d9f0184e15fe3 | 9.0 ms | wave32, 4x8 | a scalar loop over the wave's distinct values (`s_ff1`, `v_readlane`; 38 ballots, 16 lane reads, 42 first-lane reads) around a per-lane "while any lane" loop with an LDS exchange (`ds_wrxchg_rtn`); 13 accesses to title memory by address in the hot loop, **each followed by a write to the faults buffer in the PC's memory**; no barrier | the 13 fault writes behind a branch: none while the pages have copies |
| 715da27a0501263f | 5.4 ms | wave32, 32 | no loop, no barrier; wave compaction (`v_mbcnt`, `v_permlanex16`, `s_ff1`) as wave intrinsics; typed buffer loads and six `buffer_store_format_xyzw` a thread | unchanged (nothing of this branch's kind; its cost is its memory) |
| b96880482a17f632 | 4.4 ms | **wave64**, 8x8 | 26 waterfall loops over descriptors in the console code, already collapsed into per-lane heap handles (one first-lane read left); run as **two host waves meeting at 6 group barriers** (an LDS counter at the end) | the paired form: 0 barriers that wait (5 LDS fences) |
| 36f04a4ed0ad1d9b | 2.3 ms | wave32, 32 | 26 waterfall loops collapsed the same way; two loops over descriptor data (`GetDimensions` of a texture) left, one pass when the wave shares the descriptor; **111 accesses by address, each with a write to the faults buffer** (4 loops of 57-64) | the 111 fault writes behind branches |
| 31d703207b51549a | 1.0 ms | wave64, 16x16 | two host waves per console wave, **25 group barriers** (lane reads, masks as data) | paired form, 1 (the entry's) |
| d2b169b82f67ec5b | 0.96 ms | wave64, 8x8 | DPP and `ds_swizzle` across halves, **22 group barriers** | paired form, 0 that wait |
| fea2b7d3f17b504f | 1.5 ms | wave64, 8x8 | 5 group barriers | paired form, 0 that wait |

Mesh programs (db875f50a166bd91, df4b2c7798c14c90, 093c64546c55554e, ef5409053e14aa82,
6016f52cd61f30f6, b7a81f5b96f4ca93, cf43bdbfc75c9296, a6a6702e5c2a8ba9, 9bfc2a16c35cb95b,
30332cf87c4e0eae, e2bfb4680f41ceec): no group barrier, no LDS, 3-9 wave operations (lane
counts as bit counts), no access by address. Our amplification stage is one thread reading
three dwords. Nothing of this branch's kind to change there; their cost is the geometry and
its memory.

What was looked at and left alone:

- **EXEC.** Already structured: lane masks are per-lane booleans, `s_cbranch_execz` becomes a
  per-lane branch, and what EXEC still narrows without a branch is a guard: a `select` for a
  register write, a branch only around a store (56 guarded of 1,640 vector instructions in
  559d9f). The console runs the same work.
- **Waterfall loops.** Collapsed into one access with each lane's own heap index wherever the
  loop only picks a descriptor (all 26 + 26 above). The two left in 36f04a read descriptor
  dwords as numbers and run once when the wave agrees; not hot.
- **Loop limits.** One add, one compare and one OR per iteration (4,096 per loop in compute);
  `VICTORY_SHADER_LOOP_LIMIT=64` showed no gain before. Left as is: it is what keeps a loop
  over unmirrored data from hanging the card.
- **Scalar code per lane.** Uniform loads are made per lane but coalesce into one request a
  wave; the per-lane ballots and bit counts are single instructions on NVIDIA.
- **Register pressure.** Not measurable here (no register counts from the driver); the
  paired form raises it (see Risks).

## The change: `VICTORY_SHADER_FAST_FLOW` (translator, off by default)

`crates/victory-shader/src/emit/fast_flow.rs`. Unset or `0`: off. `1` or `all`: every part.
Otherwise a list of part names separated by commas, for runs that isolate one:

- **`faults`.** An access to title memory by address (`global_load/store/atomic_*`,
  `s_load_dword*` on a raw address: the ray-tracing walkers and the programs above) looks
  its page up in the directory and, when the page has no copy, records the page number in
  the faults buffer (`SLOT_FAULTS`) for the host to make that copy. Until now every access
  wrote that buffer, branch-free: the page number at the sink (dword 0) when the page had a
  copy. The faults buffer is a `shared_buffer` (custom heap, write-back, L0: the PC's
  memory), so every access of every wave was a write across PCIe. Now the records of one
  access are written after it, behind one branch on "some page had no copy"
  (`Lower::fast_memory`, `Lower::flush_faults`); with the pages mirrored, nothing is
  written. The pages recorded are the same. 14 compute programs of the title have such
  accesses (1,340 in all), among them 559d9f and 36f04a, and 6d7fe7745ab9c6d8 (246),
  b58eb3ca6ee46427 and 3592426e3e9142d8 (229 each).
- **`parked`.** The lanes of such an access that a guard leaves out (EXEC narrowed without a
  branch) look up the directory's last entry, which the host never fills, and so read the
  null view: nothing is fetched for them. Before, they looked up and read whatever their
  stale address named; their result was already dropped by the guard.
- **`paired`.** A console wave64 compute program on a 32-lane host translated as two host
  waves of 32 that exchange through LDS and meet at group barriers (lane reads, masks read
  as data, permutes, the title's LDS accesses) takes the paired form instead, where that
  translates: one host wave per console wave, two console lanes a thread, no barrier but the
  entry's (fxceless/victory-fork PR #854 by PiloteCode, in our tree since `victory-lod15`).
  Both forms are exact. Until now the paired form was used only where no other form
  translated or where the other was approximate. Of the title's 2,502 programs, 120 move,
  with 3,311 group barriers that wait fewer (3 to 38 a program). A barrier stops every wave
  of the group until the slowest arrives; with loads at ~1,200 cycles (Nsight) that is most
  of such a program's time, and a barrier some thread never reaches is also how a group can
  stand until Windows resets the card.

All three keep results bit for bit (tests below). The switch is part of the translation:
`translation_fingerprint` lists it (the build script picks every `"VICTORY_*"` literal of the
crate), so the shader cache keys on its value and **a run with the switch needs a warm-up run
with the same value** (the cooking run).

### Also fixed (a bug, no switch): the shader cache key of a copied target directory

`crates/victory-shader/build.rs` read its own source through `env!("CARGO_MANIFEST_DIR")`,
the directory **when the script was compiled**. A target directory copied from another
checkout (as every agent's was, from `target-main`) keeps that checkout's compiled script,
which cargo finds fresh: here it hashed `G:\GTA VI\victory-integ\crates\victory-shader\src`
(another copy hashed `wt-22d8455`'s), so `translation_fingerprint` carried the other
checkout's source hash and switch list. A build of this branch then keyed its shader cache
**without** `VICTORY_SHADER_FAST_FLOW` (a test now checks the list) and would have taken
translations made with the switch off for a run with it on, and the other way round. The
script now reads `CARGO_MANIFEST_DIR` when it runs. Builds from other branches with seeded
target directories may have the same fault: their translator changes may not change their
cache key. Building them once after this fix (or a fresh `VICTORY_SHADER_CACHE_KEY`) avoids it.

### Census (`victory translate --list --validate`, all 2,502 saved programs)

| | off | `VICTORY_SHADER_FAST_FLOW=1` |
|---|---|---|
| translated | 2,502 of 2,502 | 2,502 of 2,502 |
| validated by `dxil.dll` | 2,502 | 2,502 |
| wave64 compute programs in the paired form instead of meeting | | 120 (3,311 barriers less) |

The 16 costliest compute programs and the 11 mesh programs also made D3D12 pipelines on the
RTX 5080 with the switch (`translate --validate --pipelines`).

## Log lines

Once, at the first shader cache key, when the switch is on:

```
INFO victory_gpu::shader_cache: VICTORY_SHADER_FAST_FLOW: memory faults recorded behind a branch, unguarded lanes parked, wave64 programs paired instead of host waves meeting at barriers (a cache of its own) faults=true parked=true paired=true
```

With the shader cache line (every power-of-two frame from 256), what the switch changed in the
programs this run translated (the cache's hits were translated the same way when made, but are
not counted; after a cooking run expect small numbers in the real run):

```
INFO victory_gpu::queue: VICTORY_SHADER_FAST_FLOW in the programs translated so far (not those from the cache): accesses by address whose fault record is behind a branch, guarded ones parked, wave64 programs paired and the group barriers they no longer have parts=7 programs=2502 branched=1340 parked=12 paired=120 meetings=3311
```

(numbers as the census would give them; a run translates only what the title asks for).
The cooking run's line is the one to read: `paired` and `branched` above 0 show the parts
at work.

When the device goes, `when the host device went:` now also says
`translator VICTORY_SHADER_FAST_FLOW parts 0x7 (... programs translated with it, ... fault
records branched, ... accesses parked, ... wave64 programs paired instead of ... group
barriers)`.

`VICTORY_WAVE_WHY=1` (offline `victory translate`) prints `FAST-FLOW paired lanes instead of
<n> meetings` per program moved, or why the paired form was refused.

## Tests

- `emit::fast_flow::tests::the_switch_parses_names_and_all`.
- `emit::fast_flow_tests::fast_flow_reads_the_same_and_writes_faults_only_for_pages_without_a_copy`:
  a wave32 program with a guarded `global_load_dword`, an unguarded one half of whose lanes
  hit a page without a copy, and an `s_load_dword` by address, run on the hardware device
  and WARP with a directory, a page copy and the faults buffer bound as the host binds them:
  the same values and the same page recorded with and without the switch; with it the sink
  stays 0 (before, 3: every access wrote it). The test host gained a root UAV and an
  initialised raw UAV for this.
- `emit::wave64_tests::fast_flow_takes_the_paired_form_instead_of_host_waves_that_meet`: a
  wave64 program whose lane read and lane count translate as two host waves meeting; with
  the switch `emit` gives the paired form, without it the old form, `faults`/`parked` alone
  leave it alone, and both forms give the console's 64 lanes' values on the hardware device.
- `emit::fast_flow::tests::the_switch_is_in_the_translation_fingerprint`.
- The whole crate: `cargo test --release -p victory-shader --lib` 248 passed;
  `-p victory-gpu --lib` 190 passed.

## Risks

- **Paired form:** each thread runs two console lanes, so a program's code doubles (31d703:
  22k to 49k lines of DXIL) and its registers per thread rise: fewer threads in flight per
  SM, and longer driver compiles (a warm-up run is needed anyway). Every per-lane branch is
  taken by the wave as the console takes it (`structure_wide`), with EXEC as a guard: lanes
  idle inside branches as on the console. The title's LDS in the paired form relies on a
  host wave's threads running in step where they go the same way (the shortcut documented
  at `with_paired_lanes`), as the 36+ programs already in that form since #854 do. If a
  picture goes wrong only with the switch, run with `VICTORY_SHADER_FAST_FLOW=faults,parked`
  to rule this part out.
- **`faults`:** a fault record is now written after the access instead of before it; the host
  reads the buffer after the frame, so nothing sees the order. A diverging branch per access
  only where some lane's page has no copy.
- **`parked`:** only for guarded accesses, whose result the guard drops; the last directory
  entry must stay empty (`memory.rs` never fills it).
- **Overlap with the memory-access branch:** `Lower::memory` and `memory_flat`/`memory_smem`
  are touched by both; a merge may need hand work there. The fault records are a memory
  question as much as a control-flow one; they are here because the fix is a branch.
- No game run was made: what this gives in frames is for the global test to say.
