# Lists' scratch memory on the card, and our own card memory (branch `perf/scratch-on-card`)

Plan items 2a and 2c of `plan-fps-suite.md` (2026-11-25). Branch
`perf/scratch-on-card`, from `feature/levels-on-demand` (53a0b1a). Written
with the help of an AI assistant (Claude Code). Nothing here was measured in
the game yet: the owner runs one global test at the end. The numbers below
are from the logs of 2026-11-25 (opening cutscene, RTX 5080, play-lod
settings) or computed from them.

## 2a. The lists' scratch memory in GPU upload heaps

### What it was

Each host list hands out *scratch* memory (`HostList::scratch`, host.rs) for
what its draws make on the CPU:

- the constant buffers of descriptor tables (`draw.rs`, the table pass:
  `View::Constant` entries are copied per draw and get a dynamic CBV);
- the constant buffers of root views whose address is not 256-byte aligned
  on the host (`draw.rs` `root_view`);
- the vertices of expanded rectangles (`primitive::expand_rects`).

The chunks (256 KiB for a list's first, then 4 MiB; shared between lists by
`VICTORY_SCRATCH_POOL`, on by default) were made with `Host::upload_buffer`:
an upload heap, the PC's memory. Every shader that reads one of those
constants reads it across PCIe; Nsight has the card's shader units 5-8% busy
with an average load latency of ~1,200 cycles, and moving one 8 MiB table to
the card (`VICTORY_TABLES_ON_CARD=1`) was worth ~1 fps.

### What changed

`VICTORY_SCRATCH_ON_CARD=1` (or the live line `scratch_on_card=1` in
`VICTORY_LIVE_FILE`; `=0` turns it off again): new scratch chunks are made
with `Host::gpu_upload_buffer` (`D3D12_HEAP_TYPE_GPU_UPLOAD`: the card's own
memory, which the CPU writes through Resizable BAR). The pattern is
`VICTORY_TABLES_ON_CARD`'s (eaa5b48, after ubiquitarianism/leonida's notes on
tables in the PC's memory read 16-35 times slower on an RTX 5080).

- The switch is read whenever a list needs a new chunk (a few times a frame),
  so a live change applies to chunks made or taken from then on; a list keeps
  the chunks it holds until its reset.
- Spare chunks are kept apart by memory (`SPARE` for the PC's, `SPARE_CARD`
  for the card's): a chunk is never handed to the other kind.
- Scratch too big for a chunk (over 4 MiB, a buffer of its own until the
  list's reset) goes on the card the same way.
- Room: at most `VICTORY_SCRATCH_ON_CARD_MB` (256) of such chunks alive, and
  never when the card's use would pass 95% of its budget. Textures need the
  room (a bigger card budget for buffer copies made the lights wrong). Past
  either, or without GPU upload heaps, the chunk is made in the PC's memory as
  before: said once at WARN, counted per frame.
- **The CPU never reads that memory.** GPU upload memory is write-combined:
  a CPU read is uncached, some hundred times slower. Checked:
  - fills (`Step::Fill` -> `copy_mapped`, through `plaincopy::copy`,
    `victory_copy_try` (`rep movsb`) or `NtReadVirtualMemory`) only write
    the destination;
  - `expand_rects` reads the title's vertices and only writes the chunk;
  - the one reader was the check of fills taken ahead of flights
    (`fill_taken`, under `VICTORY_LATE_FILL_AHEAD=1`, which play-lod sets):
    it copied every 64th fill back out of the chunk. While any chunk is on
    the card it now reads the title's bytes, just copied from, instead.

Other upload-heap buffers our layer makes, looked at for the same move:

| Caller | What the card does with it | Moved? |
|---|---|---|
| `HostList::scratch` chunks and big scratch | shaders read constants, vertices, every draw | yes, `VICTORY_SCRATCH_ON_CARD` |
| strides table (`descriptor/host.rs`) | shaders read it | already, `VICTORY_TABLES_ON_CARD` |
| texture staging (`texture.rs` `staging_buffer`) | one copy into the texture per upload | no: read once by the copy engine, so the bus carries the bytes once either way, and it would take card memory from the textures it fills (40 GB uploaded over a run) |
| `HostList::upload` (texture copies from buffers, zeros for new levels) | one copy | no, same reason |
| budget in the PC's memory (`HostList::budget`) | clamp atomics | already on the card by default (`VICTORY_INDIRECT_CARD_BUDGET`, after fxceless PR #220) |
| the address directory (`memory.rs`) | shaders read it | already, `VICTORY_TABLES_ON_CARD` |

The small buffer copies of title memory (plan item 2b) are another branch's.

### Log lines

Every frame-rate line (about one a second), new:

```
lists' scratch memory per frame (VICTORY_SCRATCH_ON_CARD on): handed out 6.2 MB in 9100 asks from chunks in the card's memory,
0.0 MB in 0 asks from chunks in the PC's (read across the bus); chunks made 0.3 in the card's memory (0.9 MB), 0.0 in the PC's
(0.0 MB); 0.0 went to the PC's memory although the card's was asked for; 0.0 too big for a chunk in the card's memory (0.0 MB);
alive in the card's memory 61 chunks 74 MB (40 MB of them spare, at most 256 MB)
```

(The numbers are an example of the shape, not a measure.) The bytes and asks
are counted per list under the lock each ask already takes, and added up when
the list is reset, so they follow the frames with a lag of a submission or
two. With the switch off the line still says how much scratch the card reads
across the bus.

Once at INFO when the switch turns on (and again on every later change):
`lists' scratch chunks made from now on in GPU upload heaps ...`. Once at
WARN on the first fallback to the PC's memory.

The device-removed report (`when the host device went: ...`) now has the
scratch chunks on the card and the whole line below.

## 2c. Our own memory on the card

### The new line

`card memory of ours by kind: ...`, with the frame-rate lines when the total
moved by 32 MB or more since it was last said, else every 30 s (release
builds too; the older memory lines are in diagnostics builds only):

```
card memory of ours by kind: textures the title fills 6120 MB (15890), textures the host renders to or writes 2480 MB (512 with
the twins), level tiles given 3010 MB of 3200 MB of heaps; buffer copies 957 MB (their bytes from before 0 MB), windows in the
card's memory 0 MB, windows in GPU upload heaps 2048 MB; ours alone: depth twins 110 MB (28), lists' scratch in GPU upload heaps
0 MB (0 chunks), indirect arguments 61 MB (420), indirect budgets 26 MB (420 buffers; 0 lists in slabs), tables 9 MB (2), swap
chain 63 MB, copy queues' landing buffers 96 MB; ours alone in all 365 MB; all of it 13100 MB; the card 10900 MB in use of 11214 MB
```

(Example of the shape.) What each part is:

- *textures the title fills* and *textures the host renders to or writes*:
  the host copies of title textures (`resource::HOST_TEXTURES`, `KINDS`); the
  second without the depth twins. With levels on demand a texture's levels
  are in *level tiles* instead (`tiles::STATS`): given to textures, of the
  pool's heaps.
- *buffer copies* (`shadow::CARD_BYTES`): copies of title buffers in the
  card's memory, and their "before" bytes (only hashes with
  `VICTORY_CARD_REHASH_RUNS=1`: 0 MB in the logs, so no "before" twins to
  remove).
- *windows*: in the card's memory (`VICTORY_CARD_WINDOWS_MB`, 0 in play-lod)
  and in GPU upload heaps (`VICTORY_UPLOAD_WINDOWS_MB`, 2048 in play-lod).
- *ours alone*, counted where they are made and let go (`ownmem::Held`):
  - depth twins: the colour textures for depth textures shaders write, the
    depth-plane twins and the stencil copy buffers;
  - lists' scratch in GPU upload heaps (2a);
  - indirect arguments: each list's clamp buffer, at least 64 KiB, grown to
    its largest execution (the ones it replaced live to the reset, not
    counted);
  - indirect budgets: 64 KiB a list (see below);
  - tables (`VICTORY_TABLES_ON_CARD`);
  - the swap chain;
  - the copy queues' landing buffers (`VICTORY_QUEUE2`, at most
    `VICTORY_QUEUE2_LANDING_MB`, 96).
- *all of it* is the sum without counting the twins twice; *the card* is what
  the system says (it holds the title-independent driver memory and other
  programs' too).

### What the logs and the code say about each suspect

| Suspect | Size | Ours and unneeded? |
|---|---|---|
| "before" twins of card copies | 0 MB ("their bytes from before 0 MB": hashes only) | nothing to remove |
| depth twins | 28 alive; from the 28 descriptions in the log about **110 MB** (2560x1440 D32S8 21 MB, 2560x1440 16-bit depth-stencil 14 MB, three 2048x2048 R16 8 MB each, the rest small) | needed: the host has no unordered access on depth formats |
| the 3840x2160 surface | the **swap chain**: 2 buffers of 3840x2160 RGBA8, **63 MB** (the card's use went 51 -> 114 MB when it was made). The title presents a 3840x2160 front buffer although the display mode is 1920x1080 (`VICTORY_DISPLAY_MODE` only changes the mode list), and the swap chain copies it 1:1. The 3840x2160 viewports are the title's (only at boot, 18-31 a frame); in the cutscene the title draws at 2560x1440 | ours, but a smaller swap chain needs a scaling pass in place of `CopyResource`; ~47 MB to win, not done |
| host copies of textures | the bulk of the card (several GB); "copies left out since the start because both sides were one host texture" already removes the duplicates the layer can see | the title's data, not ours to drop |
| duplicate render targets | targets the console overlaps in one heap are separate resources here (no aliasing) | not clearly ours: aliasing them is a project of its own |
| rings | the indirect ring is off by default (`VICTORY_INDIRECT_RING`): 0 | nothing |
| pools | spare scratch chunks (PC's memory unless 2a is on, at most 128 MB each kind); texture staging pool (PC's memory, off); list pool (CPU memory) | nothing on the card |
| clamps' budgets | **64 KiB per list for 256 bytes** (a committed buffer is 64 KiB at the least): with 400-1,000 host lists that clamp, 25-64 MB | yes: `VICTORY_BUDGET_SLAB=1` |
| lists' clamp buffers | 64 KiB or more per list that clamps, kept by pooled lists | needed per list (each is transitioned on its own); the line says how much |

### `VICTORY_BUDGET_SLAB=1`

The clamps' budgets (256 bytes a list, `HostList::card_budget`) share 64 KiB
slabs, 256 lists a slab (`ownmem::BudgetSlabs`), instead of a 64 KiB
committed buffer each. The budget is only ever reached by address (root
SRV/UAV of the clamp and of the zeroing dispatch, under global UAV barriers),
never transitioned, so lists sharing a buffer change nothing of the states.
A list's place is given back when the list is dropped (every submission that
charged it has run then); the next list zeroes its place with its first
clamp, as before. Slabs are never let go. Off by default; read once, said
once at INFO; if no slab can be made, a buffer of the list's own (WARN once).

## Switches

| Switch | Default | Live | What |
|---|---|---|---|
| `VICTORY_SCRATCH_ON_CARD=1` | off | yes, `scratch_on_card=` | lists' scratch chunks (and scratch too big for one) in GPU upload heaps |
| `VICTORY_SCRATCH_ON_CARD_MB` | 256 | no | most bytes of scratch chunks on the card at once |
| `VICTORY_BUDGET_SLAB=1` | off | no | clamps' budgets in shared 64 KiB slabs |

The line of our card memory and the scratch line are logging only.

## Tests

- `ownmem::tests`: counting, when the line is due, the line's words, the
  slabs (256 places a slab, none handed twice, given back and taken again).
- `host::scratch_card_tests`: the room rule; a chunk asked for on the card
  is on the card when GPU upload heaps are there (it is, on the RTX 5080 and
  on WARP), the card reads what the CPU wrote through the mapping, and spare
  chunks never cross between the two memories.

## Review (branch `perf/scratch-on-card-review`)

An adversarial review of the two commits above, before the global test.

Checked and found sound:

- No CPU read of the chunks: the three writers (`draw.rs` table constants
  and `root_view` through `Step::Fill`, `expand_rects` through a host step)
  and `copy_mapped`'s paths (`plaincopy`, `rep movsb`, `NtReadVirtualMemory`,
  `read_pieces`/`read_sparse`) only store to the destination;
  `primitive::Kind::write` stores bytes without loading them. The diagnostics
  `fill_check` reads title memory, never scratch.
- States: GPU upload chunks are made in `COMMON` and only reached by address
  (CBVs, root CBVs, vertex buffer views); buffers are promoted implicitly,
  as the tables on the card already are. No transition names them.
- Budget slabs: every access of a clamp or the zeroing dispatch is inside
  bytes 0-20 of the list's 256 (indirect.rs), only global UAV barriers, no
  enhanced barrier names the budget. Lists are dropped or recycled only
  after their submissions ran (`list::recycle` after the wait; lists never
  run are let go unrun), as the old per-list buffer already needed.
- Lifetimes of chunks: given back at the list's reset, after its
  submissions ran, exactly as before.

Fixed (commit "Scratch on the card: count big scratch buffers ..."):

1. Scratch too big for a chunk that went on the card was not counted: the
   `VICTORY_SCRATCH_ON_CARD_MB` cap, the 95% rule's view of it and the line of
   our card memory missed it. Now counted with the chunks until the reset.
2. With the live switch off, chunks of the card's memory were still kept
   spare (up to 128 MB) though nothing would take them again: card memory
   held from the textures for the rest of the run, and a blurred on/off
   comparison. Now freed: given back with the switch off, or drained at the
   next chunk taken with it off. Said at INFO: `lists' scratch chunks kept
   spare in the card's memory freed: ... is off chunks=N mb=M`.
3. The line of our card memory and the device-removed report called
   `queue2::second()`, which makes the copy queues on first use, also on a
   device that may be gone. `queue2::made()` now reads them without making
   them.
4. The device-removed report took the slabs' lock with `lock()`; a failure
   on a thread that holds it would have deadlocked the report. `try_lock`.

Left as they are (logging only): the stencil copy buffers of depth twins are
subtracted from "textures the host renders to" and missing from "all of it"
(a few MB); replaced indirect argument buffers are not counted until the
reset.

Tests after the review: release 198 passed, 4 ignored; diagnostics on the
RTX 5080 214 passed, 5 ignored; WARP serially 213 passed, only
`depth_bounds_pipeline_loads_and_filters_draws` fails (it fails at the base
too). Run in parallel on WARP, the test process sometimes stops on an access
violation; `feature/levels-on-demand`'s own test binary does the same here, so
it is not from this branch. The GPU round trip of the chunks now also checks
that a chunk of the card's is freed, not kept, with the switch off.

## Risks

- Card memory: up to 256 MB more of the card while scratch is on it. The
  room rule keeps 5% of the budget free, and the line shows the sum. If the
  lights go wrong as with the bigger buffer budgets, lower
  `VICTORY_SCRATCH_ON_CARD_MB` or turn the live switch off.
- CPU writes through the BAR: write-combined stores of the fills (small,
  256-byte aligned) are posted over PCIe; if the submitting thread's
  "preparing copies" time grows, this is why.
- A CPU read of a chunk somewhere I missed would be very slow (not wrong):
  it would show as a jump of the preparing time with the switch on.
- The fill-ahead check reads the title's bytes instead of the chunk's while
  chunks are on the card: a change of the title's bytes in the microseconds
  between the copy and that read would be missed by the check (logging only).
- `VICTORY_BUDGET_SLAB`: if a list were dropped while a submission that
  charged its budget still ran, the next list's zeroing could cut that
  submission's budget; lists are let go only after their submissions ran, as
  their command allocators require.
