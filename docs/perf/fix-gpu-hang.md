# The card lost in gameplay (2026-11-25): what it was, and what now names it

Branch `fix/gpu-hang` (from `feature/levels-on-demand`). Written with the
help of an AI assistant (Claude Code). No game data in here: shader hashes
are only the names the logs print.

## What happened

The owner's gameplay session of 2026-11-25 (build `victory-lod21.exe`,
`tools/perf/play-lod.ps1`, RTX 5080) played about 6 minutes, at 3 to 10 fps,
and ended at 21:06:05Z with:

```
ERROR victory_gpu::host: host GPU device removed after 3 submissions sent ahead (VICTORY_RELAY=1) of 84 lists;
  no breadcrumbs (... 0x887A0004); device removed: ... (0x887A0006)
```

`0x887A0006` is `DXGI_ERROR_DEVICE_HUNG`. The layer's own report said the
submitting thread had been idle for 570 ms, four submissions were in the air
(started 11113, finished 11109), and the last things it did were sweeps: a
window of title memory let go at 05.038 and 113 host copies of textures at
05.060. There were no breadcrumbs: device-removed data was only possible in
diagnostics builds.

**The system's event log says what the layer's log could not.** The NVIDIA
driver (`nvlddmkm`) wrote, at 21:06:05 UTC:

```
05.220  event 153  Error occurred on GPUID: 100
05.712  event 13   Graphics SM Warp Exception on (GPC 1, TPC 3, SM 1): MMU Fault
05.712  event 13   Graphics Exception: ESR 0x5187b0=0x8030017 0x5187b4=0x4 0x5187a8=0x1c813b60 0x5187ac=0x1104
...     (the same on SMs of GPCs 0 to 6: "MMU Fault", "Multiple Warp Errors")
05.728  event 153  Error occurred on GPUID: 100
```

So it was **a page fault of a shader** (an access by the shader units to a
card address with nothing mapped there), on SMs of every GPC, and the last
two error registers of every SM hold the same pair (`0x1c813b60 0x1104`,
which looks like the faulting warps' program address): one shader, one wide
draw or dispatch, every warp of it. NVIDIA reports such a fault as
`DEVICE_HUNG`, the same code as a timeout. It was **not** a timeout of the
card (no "stopped responding" entry, and the device went 1.2 s after the
last normal frame line, under Windows' 2 s), and **not** a wait for a fence
or a value that never came (a queue waiting on a fence never signalled
stands forever without removing the device, and the title's value waits are
held on the CPU, `crate::queue`).

### Which setting

I cannot name the setting from this run with confidence (no breadcrumbs, no
faulting address). What the evidence says about each of the new ones:

| Setting | Can it make a shader page fault? |
|---|---|
| `VICTORY_QUEUE2=3` | No. Its waits are `ID3D12CommandQueue::Wait`s, which never remove the device, and they cannot stand for ever: a copy job only waits for direct-queue values already signalled, the direct queue only for a copy job already sent (`queue2.rs`); its buffers live past their use (landing buffers until the fence passes their read, fetches waited for on the CPU). Copy engines do not raise SM exceptions. |
| `VICTORY_ASYNC_SUBMIT=2`, `VICTORY_JOIN_PARTS=1`, `VICTORY_JOIN_MOST/LISTS` | Not by themselves. They change when lists reach the host, and so *when* the sweep and `retire` run against what is in the air: the lifetime rules (`retire`, the sweep at a moment with nothing in the air) are the place a use-after-free would come from. |
| `VICTORY_ASYNC_PRESENT=1` | Unlikely (presents only). |
| `VICTORY_TABLES_ON_CARD=1` | Unlikely: both tables are made once and never freed, and translated shaders clamp their indices into them (`SLOT_STRIDES_COUNT - 1`, `MEMORY_PAGES - 1`). |
| `VICTORY_MWAITX_SLEEP=1` | No (CPU). |
| `VICTORY_HEAP_WORKER=1`, `VICTORY_ALIAS_ONCE=1` | Were not on in that run (neither logged its line). |
| Levels on demand, the sweep | Possible: a sweep let go of 113 texture copies 160 ms before the driver's first error, and losses of runs 72, 73, 82 and 224 each fell close to a sweep too (`host.rs`'s trail notes). A sweep runs every second, so this is weak evidence. |

My reading, **low confidence (~30%)**: a use after free of something the
sweep let go of (a window or texture host copy, or the tiles of a texture
with levels on demand), in the gameplay's heavy texture streaming with the
card full ("the card has no room for new ones"); the new submit settings
changing the timing of sweeps against flights. The other candidate is a
root view read past the end of a host copy (root views have no bounds),
which in-game content reaches and the cutscene does not. The next loss will
say which: see below.

## What changed

| Switch | Default | Live | What it does | Log |
|---|---|---|---|---|
| `VICTORY_DRED=1` | off | no (before the device) | Device-removed data in any build: auto-breadcrumbs, breadcrumb contexts and page faults (`ID3D12DeviceRemovedExtendedDataSettings1`, else the first version). Our objects get names: lists `victory list #N`, `victory prelude or pass #N`, the queues, `victory tile heap #N`, title buffers and textures (as diagnostics builds did). In release builds the pipeline's name is set as a marker on a list each time it changes. | info once at start; at a loss: see below |
| `VICTORY_GPU_WATCHDOG=<ms>` | 2000 (0 off) | yes, `gpu_watchdog` | Every submission to the direct queue and every copy-queue job is noted (fence value, lists, their pipelines, the waits it carries). A thread looks every 100 ms; one still on the card past the limit is named once, and again when it finishes. Logging only. | warn per submission, per-frame line, part of the loss report |
| `VICTORY_GPU_PROGRESS=1` | off | no | Progress marks: before each list of a submission and after the last, a list of ours with two `WriteBufferImmediate` (`MARKER_IN`, `MARKER_OUT`) into a readback buffer, from a ring of 1,024 lists recorded once. The watchdog then names the list the card stands in. | in the watchdog's lines |
| `VICTORY_SWEEP_QUARANTINE_MS=<ms>` | 0 (off) | yes, `sweep_quarantine_ms` | What the sweep lets go of (windows of title memory, host copies of textures the title fills and so their tiles) is dropped no sooner than that, and only once every submission started by then has finished. | info once, per-frame line, part of the loss report |
| `VICTORY_AMPLIFY_GUARD=1` | off | no (compiled once) | With `VICTORY_INDIRECT_AMPLIFY=1`: an amplified mesh record of more than 65,535 groups runs empty instead of being cut to 65,535 (the stage has no work budget, unlike the clamp pass; the clamp already empties such records when ahead of their draws). New pipeline-library names for amplified pipelines: the first run with it makes them again. | info once |
| (always) | | | A lost device's report carries the display driver's entries of the last 30 s from the system's event log, alike ones counted. With `VICTORY_DRIVER_WATCH=1` each entry's text is logged as it comes. | part of the loss report |

None of this changes what is drawn with the switches off. The watchdog is on
by default because it only notes and logs (a lock and a few small copies
per submission, one thread that looks ten times a second).

### Should `VICTORY_DRED=1` be on by default?

Not for fps measurements: the runtime then writes a breadcrumb after every
command of every list (about 18,000 a frame here) and records every
allocation; Microsoft gives a few percent for breadcrumbs. **For the global
test, yes**: the point of that run is that a loss names itself. One phase
(or one of the two runs) without it gives its cost on this card.

## Log lines (examples)

Start:

```
INFO device-removed data on (VICTORY_DRED=1): auto-breadcrumbs, breadcrumb contexts and page faults; a lost device names the list and command it stopped at
INFO the GPU watchdog names a submission still on the card after 2000 ms (VICTORY_GPU_WATCHDOG; 0 turns it off)
INFO VICTORY_GPU_PROGRESS=1: a progress mark goes before every list and after the last one; the GPU watchdog names the list the card stands in
INFO VICTORY_SWEEP_QUARANTINE_MS: what the sweep lets go of is dropped 3000 ms later at the soonest
INFO VICTORY_AMPLIFY_GUARD=1: an amplified mesh record of more than 65,535 groups runs empty
```

Per frame line:

```
INFO GPU watchdog per frame (VICTORY_GPU_WATCHDOG=2000 ms): 6.4 submissions watched, the longest 96.3 ms on the card, 0 past the limit since the last line (0 reported in all), 4 in the air now; progress marks on (0 submissions without, so far)
INFO the sweep's quarantine per frame (VICTORY_SWEEP_QUARANTINE_MS=3000): 14.2 let go into it (5.1 MB), 13.8 dropped from it; held now 410 (180 MB)
```

A submission past the limit (one line):

```
WARN GPU watchdog: submission 11110 on the direct queue has been on the card 2010 ms (limit 2000 ms; sent 2240 ms ago), its fence at 15530 of 15531; 3 sent after it wait behind it; 52 lists;
  progress marks: the card has reached list 17 of 52 and finished everything before list 17;
  its lists: [0: #812 (9 pipelines), ..., 17: #640 (pipelines ["[compute 559d9f0184e15fe3]", ...]), ...];
  the waits it carries: [the direct queue waits for job 9120 of the copy queue (up), which was at 9119 when this was sent];
  queues: the direct queue: fence at 15530, 4 unfinished (values 15531..=15534); the copy queue (up): fence at 9120, 0 unfinished ...
WARN GPU watchdog: submission 11110 on the direct queue finished after all, 2350 ms on the card (2580 ms after it was sent)
```

A lost device (in the existing `host GPU device removed ...` line and the
"when the host device went" line):

```
...; page fault at 0x1104... ; inside freed ["title texture 0x... (... )" (type 18)] ; <near title memory copy>;
breadcrumbs (done/count, list of this report) [...]; 312 lists the card had finished, 60 it had not begun (the first: ["victory list #640", ...]);
list 'victory list #640' on 'victory direct queue' stopped at command 45 of 79: last done DispatchMesh, first not done DispatchMesh (op 42),
  the last marker before it: pipeline [mesh 6a84008fd80c7e59, pixel 64e3fe8556c88111], commands 40..52 between its barriers: ...
...; GPU watchdog: 4 unfinished; submission 11110 on the direct queue (...): lists [...]
; the display driver's entries of the last 30 s in the system's event log, oldest first, alike ones counted:
  nvlddmkm event 153: \Device\00000272 | Error occurred on GPUID: 100 x2 (first at ...) ||
  nvlddmkm event 13: \Device\00000272 | Graphics SM Warp Exception: MMU Fault x10 (first at ...) || ...
```

## The audit of waits that could stand for ever

- **`ID3D12CommandQueue::Wait`**: only in `queue2.rs` (`Lane::send`'s
  `after`, `Second::hold`). No cycle is possible (see the table above), and
  a GPU wait that is never met does not remove the device; the watchdog now
  names a submission stuck behind one, with the wait it carries.
- **Waits for title memory values** (`list::unmet_waits`, `queue.rs`): held
  on the CPU, never on the card.
- **Our shaders' loops**: the hash and compare loops step 1,024 bytes up to
  a bound under 4 GiB, the gather loop steps by its group count (at least 1,
  `clamp(1, 65535)` on the CPU), the clamp's compare-exchange loop has one
  lane succeed per round. Bounded.
- **Translated title loops**: every loop is cut per entry (pixel 128,
  vertex and mesh 256, ray walks 1,024, compute 4,096). Nested loops
  multiply (two nested compute loops: 16 M iterations a lane), and
  `VICTORY_SHADER_LOOP_LIMIT` is read only in diagnostics builds. Not
  changed here: it would mean a new translation of every shader, the
  translator is another agent's work now, and this loss was not a timeout.
- **Indirect counts**: the clamp pass bounds each list's work (2^27); the
  amplification stage (`VICTORY_INDIRECT_AMPLIFY=1`) had no bound beyond
  65,535 groups a draw: `VICTORY_AMPLIFY_GUARD=1`.

## Recommended for the two test runs

`VICTORY_DRED=1 VICTORY_GPU_PROGRESS=1 VICTORY_DRIVER_WATCH=1` (the
watchdog is on by default), `VICTORY_AMPLIFY_GUARD=1`, and
`VICTORY_SWEEP_QUARANTINE_MS=3000` (or `sweep_quarantine_ms=3000` in the
live file for a phase with and one without). The bake run makes the
amplified pipelines again under their new names once.

## Risks

- `VICTORY_DRED=1`: a few percent of card time; naming every buffer costs a
  string each.
- `VICTORY_GPU_PROGRESS=1`: one more list per list in each
  `ExecuteCommandLists`; a mark list is not reused while it may be in the
  air (it is skipped then, counted).
- `VICTORY_SWEEP_QUARANTINE_MS`: memory held that much longer (the card is
  already full in gameplay: texture copies held 3 s longer can delay new
  uploads).
- `VICTORY_AMPLIFY_GUARD=1`: a genuine record over 65,535 groups would draw
  nothing instead of part of it (no console dispatch runs more).
- The watchdog's report reads lists' pipeline names at submission (a copy
  of at most 24 names a list).

No upstream PR was ported here; the waits audited come from fxceless's
upstream and tibzejoker's PR #7 (`VICTORY_ASYNC_SUBMIT=2`, `JOIN_*`,
`ASYNC_PRESENT`). DRED and `WriteBufferImmediate` markers follow
Microsoft's documentation of device-removed extended data.
