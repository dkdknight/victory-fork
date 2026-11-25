# Read-only copies on the card (plan item 2b)

Branch `perf/small-copies-on-card`, from `feature/levels-on-demand`.
Switch: `VICTORY_READONLY_COPIES_ON_CARD=1` (off by default; live key
`readonly_copies_on_card`). Code: `crates/victory-gpu/src/shadow/readonly.rs`,
with small hooks in `shadow.rs` and `host.rs`.

## Why

Host copies of title memory in the PC's memory are read by shaders over PCIe,
about a microsecond a load. In the owner's runs of 2026-11-25 the frame line
"buffer copies asked for per frame by where the card reads them" counted
28-42 k asks a frame to "the PC's memory" (gameplay log `us.j`: 31-65 k, beside
45-80 k to GPU upload heaps and 130-210 k to the card's own memory). Those
copies are:

- windows outside title buffers that came too late for
  `VICTORY_UPLOAD_WINDOWS_MB`: the card was past the windows' line (80% of the
  budget) when they were made. In `us.j` the system also cut the card's budget
  from 15,209 MB to 10,989 MB at frame 1,735, right after the burst of 448
  upload windows (1,856 MB), so later windows stayed in the PC's memory;
- title buffers under `VICTORY_CARD_BUFFERS_MIN_KB` (512 KiB in `play-lod.ps1`),
  and big ones turned down for room.

Windows are never written by shaders ("0.0 MB compared in 0 ranges of
windows"), and many small buffers are only read. A copy that is only read
needs no CPU side and no compare: one buffer in a GPU upload heap (the card's
memory, written by the CPU through Resizable BAR) is enough, as
`VICTORY_UPLOAD_WINDOWS_MB` already does for windows made early.

## What it does

Once a second, from the host's sweep (`readonly::tick`, before a submission
on the submitting thread):

1. Every copy in the tables (`BUFFERS`, `LOOSE`) is looked at once: its asks
   since the last look (`Shadow::asks`, counted in `note_where`, i.e. every
   `find`/`find_any`).
2. Candidates: copies in the PC's memory (not on the card, not in place, not
   already in an upload heap), seen by an earlier look, never declared written
   by shaders (`Shadow::written`, set by `gpu_writes`/`writing`), asked for at
   least `VICTORY_READONLY_COPIES_MIN_ASKS` times a frame, at most a quarter of
   the budget (and 256 MiB), and not moved back before (`NEVER`).
3. Hottest first (`plan`): a candidate is moved if it fits under the card's
   line and under the budget. If it does not, cold copies in upload heaps
   (windows made by `VICTORY_UPLOAD_WINDOWS_MB` and copies moved earlier) asked
   for at most a quarter as often, shown by no descriptor slot and standing for
   no older copy, are put back in the PC's memory to make the room (coldest
   first). So the card's use does not grow when room is traded: the textures
   keep theirs. Only copies moved by this module give budget back.
4. Moving = making a twin in a GPU upload heap with the same span, and putting
   it in the tables in place of the old copy (`put_in_place_of`, checked under
   the tables' write lock). Every ask from then on finds the twin; its first
   refreshes fill it from title memory as for any new copy.
5. Moves and put-backs together are at most `VICTORY_READONLY_COPIES_PER_LOOK`
   a look, and making buffers stops after `VICTORY_READONLY_COPIES_LOOK_MS`.

### Writes declared later (fallback)

The first time shaders are declared to write a moved copy (`gpu_writes`, while
a list is recorded) it is moved back at once: a copy in the PC's memory takes
its place in the tables, and that title memory is never moved again (`NEVER`).
Lists recorded before still have it; their submission takes the snapshot and
compares by reading the upload heap (slow, but right: as windows in upload
heaps have always done). This is counted ("moved back for shader writes",
"snapshots read from GPU upload heaps") and warned.

### Descriptor slots that show the old copy

A slot made on a copy keeps that copy's buffer, but the pass over the heap
(`draw.rs`, through `descriptor::host::buffer_copy`) brings up to date the
copy the tables hold now. Without care, a slot made before a move would read a
copy nothing refreshes. So the new copy keeps the old one (`Shadow::forwards`):
every refresh and every declared write asked of the new one is asked of the old
one too (`refresh`, `refresh_many`, `gpu_writes`). The old one is let go once
nothing but the new one holds it and no slot shows its buffer
(`readonly::cleanup`, each look; chains pass to the newest). `let_go_idle` does
not let go a window that stands for older copies. Lists that bound the old copy
keep refreshing it themselves, as when a window is let go.

## Switches

| Switch | Default | What |
|---|---|---|
| `VICTORY_READONLY_COPIES_ON_CARD` | off | `1` turns it on. Live: `readonly_copies_on_card=1/0` in `VICTORY_LIVE_FILE` (off stops moving; moved copies stay). |
| `VICTORY_READONLY_COPIES_MB` | a 32nd of the card's budget at the first look, 64-512 MB (475 MB on the 5080's 15,209 MB) | the most bytes of title memory in moved copies |
| `VICTORY_READONLY_COPIES_PERCENT` | `VICTORY_CARD_WINDOWS_PERCENT` (80) | the line of the card's budget under which a copy moves without trading room (50-97) |
| `VICTORY_READONLY_COPIES_MIN_ASKS` | 1.0 | asks per frame a copy needs |
| `VICTORY_READONLY_COPIES_PER_LOOK` | 16 | moves + put-backs per look (1-256) |
| `VICTORY_READONLY_COPIES_TRADE` | on | `0`: never put cold copies back to make room |
| `VICTORY_READONLY_COPIES_LOOK_MS` | 4 | ms a look may spend making buffers |

Nothing changes with the switch off: the ask counter and the written flag are
counted but not used, and no copy ever stands for an older one.

## Log lines

Once, when on (info):

    VICTORY_READONLY_COPIES_ON_CARD=1: copies shaders only read move from the PC's memory to GPU upload heaps, hottest first:
    at most 475 MB (VICTORY_READONLY_COPIES_MB; the card's budget at the first look 15209 MB), under 80% of the card's budget
    (VICTORY_READONLY_COPIES_PERCENT), 16 moves a look (VICTORY_READONLY_COPIES_PER_LOOK), from 1.0 asks a frame
    (VICTORY_READONLY_COPIES_MIN_ASKS); cold ones put back to make room: on (VICTORY_READONLY_COPIES_TRADE)

The first 16 moves, then every 64th (info):

    a copy shaders only read moved to a GPU upload heap (VICTORY_READONLY_COPIES_ON_CARD) n=3 start=0x... bytes=4194304
    window=true asks_per_frame=212.0 shown_by_slots=false frame=1800

Every frame line (beside "buffer copies asked for per frame by where the card
reads them", which should now show asks moving from "the PC's memory" to "a GPU
upload heap"):

    read-only copies moved to the card's memory per frame (VICTORY_READONLY_COPIES_ON_CARD): 1.20 moved (4.8 MB), 0.40 cold
    ones put back in the PC's memory (1.6 MB), 0.00 moved back for shader writes, 0.00 snapshots read from GPU upload heaps;
    6.3 MB written into them over the bus by refreshes; turned down 0.00 for room on the card, 0.00 over
    VICTORY_READONLY_COPIES_MB; 2.1 could move, 0.3 hot ones written by shaders; now 96 moved alive (384 MB of at most
    475 MB), 12 copies standing for older ones a slot may show (840 refreshes and writes passed on, 0.10 older let go per
    frame); looks 0.21 ms per frame, 0 raced

Read it as: "moved" and "MB written into them" are the bytes taken off the bus;
"turned down ... for room" means the card was past its line with nothing cold
enough to trade; "hot ones written by shaders" are hot copies this cannot help;
"copies standing for older ones" with many "passed on" means slots still show
old copies (those reads still cross the bus, and their refreshes are done
twice) until the title writes those slots again.

Warnings (fallbacks): no GPU upload heaps; the system made no upload buffer; a
moved copy written by shaders (first 8, then powers of two); snapshots read
from upload heaps (first 4, then powers of two).

Each move and put-back goes to the trail ("read-only copies: title memory
... moved to a GPU upload heap"), and the device-removed report ("when the host
device went") has a part "read-only copies in GPU upload heaps (...): N alive
(MB); since the start ... moved, ... put back cold, ... moved back for shader
writes; ... copies standing for older ones; the last look at frame F moved m,
put back p, turned down t".

## Risks

- Textures' room: copies move only under the line (80% by default) or by
  trading room with colder upload copies; the most is capped. The texture
  queue still fills to its own line, so the room taken under 80% is room a
  later texture does not get; `VICTORY_READONLY_COPIES_MB` bounds it (lower it,
  or set `VICTORY_READONLY_COPIES_PERCENT` lower, if lights go wrong as with
  bigger card-copy budgets before).
- For one frame at a move, two copies of the same memory exist: one bound by
  lists recorded before, one after. Both are refreshed from title memory before
  their submission. A shader write declared on one, in the same submission as a
  read of the other, would not be seen by the reader (the same as overlapping
  windows today). Only at the moment of a move back for writes.
- Old copies kept for slots: refreshed twice while slots show them; if the
  title keeps such slots for long, "copies standing for older ones" stays high
  and the gain is smaller for those copies.
- Reads not declared: a shader reading a copy through a path that does not
  refresh it (none known; root views, tables, the heap pass, the raw address
  directory and indirect arguments all refresh) would see zeros in a new twin
  where the old copy had bytes from an earlier refresh.
- The first refresh of each twin writes its asked range over the bus (up to
  16 x 4 MiB in the frame after a look).
- `shadow::near` (device-removed report) names copies in the tables, not the
  older ones kept for slots.

## Tests

`cargo test -p victory-gpu --lib readonly`: the plan (hottest first, only
colder copies traded, budget only given back by moved copies, the per-look
limit, the default budget), and on the software adapter: a copy takes the
place of one the tables hold, and an older copy stays while a slot shows its
buffer and is let go after (chains pass to the newest).

## Credit

GPU upload heaps for host copies follow this fork's own
`VICTORY_UPLOAD_WINDOWS_MB` and `VICTORY_TABLES_ON_CARD`; the ask counts are
those of the "where the card reads them" line (`VICTORY_WHERE_TOP`).
