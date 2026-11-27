# The title's two frames in flight, and what keeps it there (1.0.2.1)

Research notes from reverse engineering `game_scarlett_master.exe` 1.0.2.1
(Ghidra headless + capstone) and from crash dumps, on an RTX 5080 with the
coolfox base, the city save as the test scene. Work in progress, written by an
AI agent (Claude) for dkdknight; the frame limiter and the first crash at
+0x4e9b4b2 were found by Thibault's agent (tibzejoker). Addresses are offsets
from the image base.

## Why it matters

The title ends each frame in a hard-coded limiter of two frames in flight. In
the city its wait is about half of the frame (33.6 ms of ~65, measured by
Thibault's agent), so the frame period is one frame's whole latency
(preparing, the card, the landing) instead of the slowest of them. A third
frame in flight would overlap them.

## The pieces found

| Where | What |
|---|---|
| `+0x2474740` | Frame limiter: ring of 2 tokens at `+0x75DDB60` (`and eax, 1`), tail `+0x75DDB74`, head `+0x75DDB78`; waits on the oldest (`+0x4fb0a50`) while `head - tail >= 2`, then drains the deferred-release ring. |
| `+0x75D5B60` | Deferred-release ring, 0x1000 object pointers, consumer `+0x75D5B50`, producer `+0x75D5B54` (`+0xbfb010` pushes); a null entry closes a frame (pushed by the limiter after its drain). |
| `+0x5c280` | Command blocks: an LRU list of 128 KiB blocks (descriptors of 0x58 bytes), recycled when their fence has passed (`+0x4dad000` / `+0x2474880`, real fence checks), never the two newest (`cmp ebp, 2` at `+0x5c2d0`). |
| `+0x5c5ff00` | Block count and size, set once at start (count `+0x75df1c8`: 216 here; size `+0x75df1cc`: 0x20000). |
| `+0x161cef0` | Builds the two pools of command allocators (`+0x75df220`, `+0x75df240`): one 0x30-byte wrapper per block, head index in the low half of the next word, next index at `+0x2c`. |
| `+0x188430` / `+0x2474a90` | Pop a wrapper (no empty check). |
| `+0x4e9b480` / `+0x50b8970` → `+0x4e9b530` | When a block is reused, each node `{next, wrapper, version}` it holds resets that allocator (`vtable+0x40`) and pushes the wrapper back. |
| `+0xA9437AC` | Frame counter: 140 read sites, 10 parity sites (`frame & 1`). |
| `+0x8759f70`, `+0x8d44b68`, `+0x8d44de8`, `+0x8d45068`, `+0x8d452e8` | Five arrays doubled by frame parity (one of 2x16 bytes, four bitmaps of 2x256 bytes with a 16-bit header; cleared each frame by `+0x550fe0`). |

## Finding 1: the allocator pool runs dry, even with two frames

A wrapper comes back only when a block that noted it is recycled, which is a
whole turn of the LRU list later. At about one wrapper taken per frame, the
first pool is empty after ~240 frames, already in the menus, with the title's
own limiter (counted every 120 frames from our hook at the frame end). A pop
from the empty pool takes the word one past the array: 0x900000000 here, the
start of the next memory region. Later, `Reset` through a null allocator at
`+0x4e9b578`. That is the crash Belobug #583 reports under Linux; with three
frames in flight it comes within a minute.

Fix (`VICTORY_MORE_ALLOCATORS=N`, on by default at 4 with
`VICTORY_FRAMES_AHEAD=3`): the `mov r13d, [count]` at `+0x161cf00` becomes a
call to a stub near the image that multiplies the count. Measured: the first
pool stays at 648 of 864 free; with two frames in flight, the city runs with
no crash at 10-11 fps (unchanged).

## Finding 2: three frames in flight (`VICTORY_FRAMES_AHEAD=3`)

Patches, each can be left out with `VICTORY_FRAMES_AHEAD_SKIP=i,j`:

0. the limiter's ring moved to four slots of ours near the image;
1. and 3. `and eax, 3` for the head and tail slots;
2. and 4. wait from three in flight, down to two;
5. keep three blocks instead of two (`+0x5c2d0`);
6. our deferred-release drain at `+0x24747e6`, one frame later than the
   title's (`VICTORY_FRAMES_AHEAD_DRAIN=own` gives the title's timing);
7. more command blocks (`VICTORY_FRAMES_AHEAD_BLOCKS`, 1 by default: more
   blocks did not help, the pool leak scales with them).

State: with the allocator fix, the title runs past the menus and the loading
(~150 s instead of 60), then a command block is overwritten when the city
appears (`+0x4e9b54a` / `+0x4e9b4b2`, invalid node in a block's list). It is
not patch 5. Suspects next: the five parity arrays, and anything else indexed
by `frame & 1`.

## How to reproduce

The census line `frames ahead: the title's command blocks` (every 120 frames)
prints the block count and the free wrappers of both pools. The title's own
crash report (`crashcontext.log`) and minidump land in the save's `temp`
folder. Its exception context gives the faulting node.
