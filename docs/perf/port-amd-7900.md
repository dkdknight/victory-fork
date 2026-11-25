# Fixes ported from avatarmanz/VIctory-7900XTX

Branch `port/amd-7900` (from `integ/fps-15` at 1060e72). Written with the help
of an AI assistant (Claude Code). No game data in here.

avatarmanz/VIctory-7900XTX is another derivative of fxceless/victory-fork,
worked on with an RX 7900 XTX from 2026-11-23 to 2026-11-25. Both code bases
left fxceless about 175 commits ago each, so nothing cherry-picks: each item
below was read in their commit, and the mechanism written again on our code.
Their scripts were not run. Every item is behind a switch of its own and off
by default; none changes anything while its switch is off, except that the
tests below were added.

## Switches at a glance

| Switch | Default | Live key (`VICTORY_LIVE_FILE`) | Their commit |
|---|---|---|---|
| `VICTORY_SEND_OWN=1` | off | `send_own` | 5b422ba, 18c0129 |
| `VICTORY_CARD_ALL=1` | off | no (when a copy is made) | 18c0129 |
| `VICTORY_CARD_HASH_LEN=1` | off | no | 17c4717 (after fxceless c36971b2) |
| `VICTORY_KEEP_FREED=1` | off | `keep_freed` | 7d3e179 |
| `VICTORY_WAIT_RESIDENT=1` | off | no | 0322b39, f1d3eb2 |
| `VICTORY_VIEW_COPY=1` | off | no | 02f36e4 |
| `VICTORY_CARD_COMPARE_EIGHT=1` | off | no (when the compare is made) | 9b90ad1 |
| `VICTORY_TEXTURE_BACK=1` (or a list of kinds) | off | no | 23925a4 (fxceless f5c03f3b) |

Their defaults differ: there all of these are on. Names kept where they do not
clash with ours. Four of their names mean something else here and were not
used with their meaning: `VICTORY_ASYNC_SUBMIT`, `VICTORY_JOIN_PARTS`,
`VICTORY_FAST_CORES`, `VICTORY_CARD_BUFFERS_MIN_KB` (ours keeps its floor of
one 64 KiB piece; their "any size" is what `VICTORY_CARD_ALL` does for the
small ones).

## Item 1: small card copies overwritten under earlier submissions

### VICTORY_SEND_OWN (their 5b422ba "wip", 18c0129)

What it does. A copy of a title buffer in the card's memory has two sides: the
CPU side in the PC's memory, which refreshes write, and the card side shaders
use. Before each submission `Host::card_before` takes the ranges that changed
(`shadow::card_dirty`) and records copies from the CPU side to the card side.
Those copies run when the card comes to the submission, and with the relay
(on by default) that is a frame or two after it was prepared: later
submissions' refreshes have written the CPU side meanwhile, and the earlier
submission's shaders read the later bytes. For what the title's CPU writes
that is as on the console, but the title's buffer copies are commands in its
lists that we do when a submission is prepared: "a value to a buffer, a draw
that reads it", twice with two values, and both draws read the second.

What we changed. `crates/victory-gpu/src/host/send_own.rs` (new): with the
switch, `card_before` copies the submission's changed ranges out of the CPU
sides into upload memory of the submission's own (packed in the order the
`VICTORY_QUEUE2` landing buffer has them), and the sends copy from there: on
the direct queue, or as one copy into the landing buffer on the copy queue.
The memory is held by the `CardPlan` and comes back in `card_done`, when the
submission has run; up to 512 MB of it is kept spare. Not taken from theirs:
the scatter pass that stores runs of a few words as offset/value pairs (we
keep one `CopyBufferRegion` per range, as before), the `touched` flag, and
`send_copied` (sending a title buffer copy's destination whether the CPU side
changed or not: our refreshes mark the whole range they copy as changed).

Log lines. Once: `VICTORY_SEND_OWN=1: what goes up to copies in the card's
memory is taken out of their CPU sides ...`. Per frame, folded into the line
`host copies in the card's memory: ...`: `; sent from the submissions' own
memory (VICTORY_SEND_OWN): N MB in N ranges, N ms taking them, N buffers made,
N MB spare`.

Risks. CPU time on the preparing thread (the bytes are copied once more; over
4 MiB a submission on the kept threads). Upload memory: up to the bytes in the
air plus 512 MB spare. The bytes are read from the CPU side while the relay's
landing thread may merge into it; what a merge puts in is in the dirty list
again and goes with the next send, as before.

Test. `cargo test -p victory-gpu --features diagnostics each_reads_its_own --
--nocapture` with `VICTORY_D3D12_WARP=1 VICTORY_CARD_BUFFERS_MIN_MB=1
VICTORY_SEND_OWN=1` and `VICTORY_RELAY=2` or `VICTORY_LATE_WAIT=3`
(`host::queue2_tests::warp_each_submission_reads_the_bytes_it_was_prepared_with`):
six submissions read one dword the title writes anew before each; each must
read its own. Without `VICTORY_SEND_OWN=1` and with submissions sent ahead it
shows the fault. In the game: the black picture with
`VICTORY_CARD_BUFFERS_MIN_KB=64`, and lighting with 3072 MB budgets.

### VICTORY_CARD_ALL (their 18c0129)

What it does. A copy with one side (title buffers under the least size, 1 MiB
by default) is what the card reads while our refreshes for later submissions
write it. With the switch every title buffer's copy has two sides: the card
side in the card's memory while there is room and the copies stay under
`VICTORY_CARD_BUFFERS_MB`, else a second buffer in the PC's memory. Each takes
at least one 64 KiB piece. No title buffer is placed in title memory itself
(`VICTORY_INPLACE`) then.

What we changed. `shadow.rs`: `card_all`, `card_side_for` (the PC fallback),
`Card::pc_side` and its own byte count, the least size 0 in `card_for`, the
info line per copy only for the big ones.

Log lines. Once: `VICTORY_CARD_ALL=1: every title buffer's copy has a CPU
side and a card side, ...`. In the line `host copies in the card's memory`:
`; card sides in the PC's memory (VICTORY_CARD_ALL): N MB`. The lines `no
card copy for a title buffer of N KiB` (first 64, then every 32nd) say which
fell back.

Risks. Card memory: a piece per small buffer (thousands); the textures' room
shrinks by that (`VICTORY_CARD_ROOM_FOR_BUFFERS` counts it). More copies to
hash and compare after submissions that declare writes. On its own it only
gives the small copies the same timing as the big ones; it is with
`VICTORY_SEND_OWN=1` that they read their own submission's bytes.

### VICTORY_CARD_HASH_LEN (their 17c4717, after fxceless c36971b2)

What it does. A card side is the title buffer rounded up to 64 KiB; the
card's hashes, compares and in-flight fetches went over all of it. With the
switch they stop at the title buffer's end (16-byte step). What lies past the
end is never sent up nor merged back (`card_piece` cuts there), so only the
work changes: a copy of 288 bytes hashes 288 bytes, not 64 KiB.

What we changed. `Shadow::card_hash_size`, used for the size passed to the
rehash after sends, the compare after the lists and `CARD_FETCH`. Their
"copies down to any size" (`VICTORY_CARD_BUFFERS_MIN_KB` without a floor) is
not taken: see `VICTORY_CARD_ALL`.

Log line. Once: `VICTORY_CARD_HASH_LEN=1: ...`. Test:
`shadow::card_tests::hashes_stop_at_the_title_buffers_end`.

Risks. None known; the "before" hashes are made with the same size (the
switch is read once).

### How item 1 meets what we have

- Relay copy-back: unchanged. Sends are numbered as before (`card_dirty`'s
  `sent` notes); a merge skips ranges sent after its fetch's send whether the
  card got the bytes from the CPU side or from the submission's own memory.
- Card hash compare: the rehash after the sends hashes the card side after
  the copies, as before; with `VICTORY_CARD_HASH_LEN=1` both the hash and the
  compare stop at the buffer's end.
- `VICTORY_READONLY_COPIES_ON_CARD`: its twins in GPU upload heaps have one
  side, written in place by refreshes: the same hazard, and `VICTORY_SEND_OWN`
  does not cover them. It moves only copies with one side, so with
  `VICTORY_CARD_ALL=1` it has only windows outside buffers left to move.
  Test item 1 without it first.
- `VICTORY_TABLES_ON_CARD`: our strides table and address directory, not
  title buffer copies; written in place as before, untouched by item 1.

### Not ported: VICTORY_COPIES_IN_PLACE (their 350f7f4)

Title buffer copies done on the card at their place in the list (copy, draw,
copy, draw within ONE submission: both draws read the second here). It needs
a new kind of submission step with its own waits for flights (`blocked_by`),
the reference of the destination's copy (CPU side or snapshot) set to the
copied bytes under the merge lock so that the landing does not take them for
shader writes, scratch memory addressable as a buffer, and their
`card_steps` planner. Each of those touches the copy-back's correctness, and
none can be checked here without the game; `VICTORY_SEND_OWN` covers the
case across submissions, which is the one of the black picture and the
budgets. Left for a branch of its own.

## Item 2: stability

### VICTORY_KEEP_FREED (their 7d3e179)

What it does. A shader that reads past a view, or through a slot that shows
stale data, reads whatever lies there and nothing happens; where memory was
given back it is a page fault and the device is lost. Their two losses of
2026-11-25 were reads in memory given back seconds before; ours of 21:06 came
0.2 to 0.7 s after the sweep let go of a window and 113 host textures. With
the switch: host textures idle five minutes (not 1-5 s) while the card is
under 85 % of its budget (`vram_idle_ms`); windows, staging buffers and the
pipeline library's mapping five minutes while 12 GiB or more of the PC's
memory are free (`ram_idle_ms`); spare scratch chunks in the PC's memory up
to 1 GiB (not 128 MiB). Over those lines the idle times are as before.
Their empty texture blocks have no counterpart: our textures are committed.

Against `VICTORY_SWEEP_QUARANTINE_MS` (fix/gpu-hang): the quarantine delays
the drop after the sweep has let something go; this delays the let-go itself.
They add up, and are kept apart so that each can be tried alone.

Log line. Once: `VICTORY_KEEP_FREED=1: idle host textures stay 5 minutes ...`.
The sweep's and the quarantine's lines show how much goes. Tests:
`host::batch::vram_idle_ms_scales_with_pressure`,
`ram_idle_ms_scales_with_pressure`. Risk: memory held longer (card under
85 %, PC with 12 GiB free); a texture that is in fact used again is found
(no upload anew).

### VICTORY_WAIT_RESIDENT (their 0322b39, f1d3eb2)

What it does. A committed resource is resident once made, but its mapping
into the card's address space is work the system does later; only lists sent
after it wait for it. Lists already on the card that read a descriptor slot
written now (bindless) find nothing mapped there. With the switch host
textures (not reserved ones), depth twins, unordered access buffers and
shared buffers are made `CREATE_NOT_RESIDENT`, made resident, and a copy of
16 bytes on a copy queue of ours is waited for after it (the system runs it
only once all mappings asked for before are done; `MakeResident` alone came
back early in their run n11). Upload buffers are left out: only lists sent
after them can reach them.

What we changed. `crates/victory-gpu/src/host/resident.rs` (new); the
creation sites in `host.rs` (`shared_buffer`, `uav_buffer`) and `resource.rs`
(host textures, twins). Log lines: once `VICTORY_WAIT_RESIDENT=1: ...`; at the
first and every 2000th wait `new card memory waited for until it is resident
and mapped (VICTORY_WAIT_RESIDENT=1), since the start: N times, N ms, the
longest N ms, N failed`. Risk: a tenth of a millisecond and more per object,
on the thread that makes it (texture bursts when the world loads).

### VICTORY_VIEW_COPY (their 02f36e4)

What it does. On their card a thread that wrote views over and over in a slot
of a shader-visible heap let the card see a descriptor that was neither the
old one nor the new one in 4 % of its reads; copied from a heap the card does
not see, none in 8.5 billion. With the switch every slot `descriptor/host.rs`
makes (views, null views, constant buffers, samplers) is made in a slot of the
thread's own in a CPU-only heap (4096 threads at most; more make them in
place) and copied in with `CopyDescriptorsSimple`. A making nested in another
on the same thread (a view whose making makes another) is done in place.

What we changed. `HostHeap::making` and `Making` (put / drop), `staged`,
`view_copy` in `descriptor/host.rs`; every `self.cpu(index)` that a descriptor
was made at now goes through it. Their raw twins have no counterpart here.
Log lines: once `VICTORY_VIEW_COPY=1: ...`; in the five-second block `host
descriptors made beside the heaps and copied in since the start
(VICTORY_VIEW_COPY=1): N`. Risk: whether NVIDIA writes descriptors in steps
is not measured; the cost is one copy per slot made.

### Not ported: VICTORY_SHOW_AFTER (3bab23c) and VICTORY_RIPE_BLOCKS (3b73cb0)

Both guard their own texture allocator (`pool.rs`: placed textures cut from
64 MiB blocks, pieces given back and reused): a slot shows a neutral texture
until the upload of a texture made in a block in the last second has run, and
textures are cut only from blocks older than every list on the card. Our host
textures are committed resources (each its own memory, resident when made;
with `VICTORY_WAIT_RESIDENT=1` also mapped), and the reserved ones of
`VICTORY_LEVELS_ON_DEMAND` get their tiles in a step ahead of the upload and
show only levels written whole (`tiles.rs`). There is no block to be young,
and a slot that shows a texture before its upload reads old texels, not
unmapped memory.

## Item 3: not ported: VICTORY_INDIRECT_BATCH (0375962)

One pass clamps the arguments of every indirect execution between two points
where they may change, reading its jobs from a table in upload memory the CPU
fills while the list is recorded (their replay: 2,355 clamps to 980 passes,
86.1 to 77.7 ms). Ours clamps through `crate::indirect` forms of its own (the
ring, the prelude that clamps records the list did not write before it runs,
single-lane clamps) and windows with remembered steps
(`VICTORY_INDIRECT_WINDOW`, `VICTORY_WINDOW_JUMP`), with another layout of the
budget words. Their batch sits on their window code and budget layout; here
it would be a fourth way beside three, with its own state ("buffers written"
on dispatches and storing draws, barriers and pipeline syncs from
`list.rs`). It is a gain in time, not a fix, and a wrong record is a hang:
left for a branch of its own, measured against `VICTORY_INDIRECT_PRELUDE` and
`VICTORY_WINDOW_JUMP`.

## Item 4: VICTORY_CARD_COMPARE_EIGHT (their 9b90ad1)

The compare of card copies loads eight places of a piece at once and mixes
them in the same order: the same hash, the loads issued together (their yard
capture: 1.93 to 1.69 ms for 423 passes a frame). `CARD_COMPARE` has the loop
under `#ifdef EIGHT`; `card_compare_source` defines it with the switch. Log:
once `VICTORY_CARD_COMPARE_EIGHT=1: ...`. Test:
`indirect::tests::warp_card_compare_eight_at_once_makes_the_same_hashes`
(WARP: both make the same hashes, whole pieces, skewed ones, a 288-byte
copy). Risk: none known (same results); gain unmeasured on NVIDIA.

## Item 5: VICTORY_TEXTURE_BACK (fxceless f5c03f3b, their 23925a4)

The ocean's height and displacement maps are textures compute passes write
and the title's physics samples on the CPU; here they stayed in the host
copy, and boats sank. With the switch a list whose shaders write a texture
of a listed kind through unordered access copies it into a read-back buffer
when it closes; once it has run (in a waited submission, or when the relay
lands its flight) the texels are tiled into the texture's title memory in
the console's layout. With the relay that memory counts as written by the
flight until it lands, as declared buffer writes do. A read-back is dropped
when the texture is released, another is made at its address, or the
runtime maps or unmaps memory there.

`VICTORY_TEXTURE_BACK=1`: the four water kinds of the upstream trace
(`256x256f41,256x256f16,64x64f16,128x128f36`); a comma-separated list of
`<width>x<height>f<format>`: those; unset or `0`: none (theirs is on by
default). New: `crates/victory-gpu/src/texback.rs`; hooks in `draw.rs`
(`readable`), `host.rs` (list close, `Sent`/`Flight`/`Line`, read-backs),
`resource.rs`, `watch.rs`. Log lines: once the kinds; at the first and every
1024th landing `texture read-backs landed in title memory since the start
(VICTORY_TEXTURE_BACK): N of N recorded, N MB; N dropped (memory changed), N
not tiled`. Tests: `texback::tests`. Risk: about a megabyte copied back per
list that writes such a texture, whether the title reads it or not; a title
read before the landing sees the frame before.

## Not taken

AMD-only items (RDNA float rules 20b9834, `INDIRECT_WIDE` 84eae35, native
wave64) and their CPU-side work (`VICTORY_THIN`, `CARD_STEPS`, `LAND`, ...).

## Tests run

With `CARGO_TARGET_DIR=G:/GTA VI/target-amd-port`, `-j 8`, on 2026-11-26:

- `cargo build -p victory --release`, and with `--features diagnostics`: no
  errors, one warning that was there before (`draw.rs` `memo_say`).
- `cargo test -p victory-gpu --lib --release`: 232 passed, 5 ignored.
- `cargo test -p victory-shader --lib --release`: 269 passed, 2 ignored.
- `host::queue2_tests` on WARP (diagnostics, `VICTORY_D3D12_WARP=1
  VICTORY_CARD_BUFFERS_MIN_MB=1 VICTORY_RELAY=2`):
  `each_reads_its_own` with `VICTORY_SEND_OWN=0`: 4 of 6 rounds read another
  round's bytes (the fault, shown); with `=1`: none. With `VICTORY_SEND_OWN=1
  VICTORY_CARD_HASH_LEN=1 VICTORY_CARD_COMPARE_EIGHT=1 VICTORY_WAIT_RESIDENT=1
  VICTORY_VIEW_COPY=1` all three tests pass with `VICTORY_QUEUE2` 0, 2 (sends
  through the copy queue's landing buffer: 18 of them) and 3.

Not run: the game (nothing here has been seen in a picture yet).

## A global test

One run with item 1 and the safe parts of item 2 together, on top of
`tools/perf/play-f15.ps1`'s settings:

```
VICTORY_SEND_OWN=1
VICTORY_CARD_ALL=1
VICTORY_CARD_HASH_LEN=1
VICTORY_CARD_COMPARE_EIGHT=1
VICTORY_KEEP_FREED=1
VICTORY_WAIT_RESIDENT=1
VICTORY_VIEW_COPY=1
VICTORY_TEXTURE_BACK=1
```

and without `VICTORY_READONLY_COPIES_ON_CARD`. Then the two that failed
before: `VICTORY_CARD_BUFFERS_MIN_KB=64` (with and without `VICTORY_CARD_ALL`)
and the 3072 MB budgets. Live during the run (`VICTORY_LIVE_FILE`, lines
`send_own=0|1`, `keep_freed=0|1`): turning `send_own` off and on shows
whether the picture follows it. The others are read once.
