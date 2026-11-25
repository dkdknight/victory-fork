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
card address with nothing mapped there), on SMs of every GPC: one wide draw
or dispatch, every warp of it. NVIDIA reports such a fault as
`DEVICE_HUNG`, the same code as a timeout. It was **not** a timeout of the
card (no "stopped responding" entry; the driver's first error came 0.7 s
after the last normal frame line, under Windows' 2 s), and **not** a wait
for a fence or a value that never came (a queue waiting on a fence never
signalled stands forever without removing the device, and the title's
value waits are held on the CPU, `crate::queue`).

**It is not new, and it is not tonight's settings.** The system's event log
holds three such losses on 2026-11-25, with the same signature (MMU faults
on SMs of every GPC, and the same values `0x1c813b60 0x1104` in the last two
error registers of every SM, in all three):

| UTC | Run | Its settings |
|---|---|---|
| 01:48:30 | run 43 ("amp2", 44 s after start) | `VICTORY_RELAY=1 VICTORY_LATE_WAIT=2 VICTORY_INDIRECT_AMPLIFY=1`: none of tonight's new switches. At the loss two pipelines had been in the making for 2.4 s. |
| 17:09:41 | the barrier-groups experiment (`b-groups`) | the play settings, `VICTORY_INDIRECT_AMPLIFY=1`; new pipelines bound in the second before. |
| 21:06:05 | the owner's gameplay session | play-lod.ps1 (`VICTORY_INDIRECT_AMPLIFY=1` and tonight's new switches); 300-580 draws a frame dropped for want of a host pipeline in gameplay. |

### Which setting

**My reading, medium confidence (~55%): `VICTORY_INDIRECT_AMPLIFY=1` (the
amplification stage for one-record indirect mesh draws), not one of
tonight's new switches.** The stage reads the draw's record when the draw
runs and cuts the group count to 65,535; a record over that is not one the
title wrote (no dispatch of the console runs more, and the title leaves all
ones in a slot it has not filled: the clamp pass already empties such
records when it clamps them ahead of their draws). When the pass that writes
the records is dropped (a pipeline the layer does not have yet: gameplay
drops hundreds of draws and dispatches a frame, and each loss came while
pipelines were being made), the mesh shader runs 65,535 groups and reads its
meshlets by group number, far past the ends of its buffers, through root
views, which have no bounds: a page fault on every SM, as logged. The
stage, unlike the clamp pass, has no work budget per list. This is what
`VICTORY_MESH_GUARD=1` stops (below). What would prove it: with
`VICTORY_DRED=1` the next loss names the list, the command
(`DispatchMesh`) and the pipeline, and the page-faulting address.

The new switches, one by one:

| Setting | Can it make a shader page fault? |
|---|---|
| `VICTORY_QUEUE2=3` | No. Its waits are `ID3D12CommandQueue::Wait`s, which never remove the device, and they cannot stand for ever: a copy job only waits for direct-queue values already signalled, the direct queue only for a copy job already sent (`queue2.rs`); its buffers live past their use (landing buffers until the fence passes their read, fetches waited for on the CPU). Copy engines do not raise SM exceptions. |
| `VICTORY_ASYNC_SUBMIT=2`, `VICTORY_JOIN_PARTS=1`, `VICTORY_JOIN_MOST/LISTS` | Not by themselves (run 43 had none of them). They change when lists reach the host, and so *when* the sweep and `retire` run against what is in the air. |
| `VICTORY_ASYNC_PRESENT=1` | Unlikely (presents only). |
| `VICTORY_TABLES_ON_CARD=1` | Unlikely: both tables are made once and never freed, and translated shaders clamp their indices into them (`SLOT_STRIDES_COUNT - 1`, `MEMORY_PAGES - 1`). Not in run 43. |
| `VICTORY_MWAITX_SLEEP=1` | No (CPU). |
| `VICTORY_HEAP_WORKER=1`, `VICTORY_ALIAS_ONCE=1` | Were not on in that run (neither logged its line). |
| Levels on demand, the sweep | Possible but less likely: a sweep let go of 113 texture copies 160 ms before the driver's first error tonight, but sweeps run every second, and run 43's let go of almost nothing (78 objects at once in the whole run). `VICTORY_SWEEP_QUARANTINE_MS` tests it. |

## What changed

| Switch | Default | Live | What it does | Log |
|---|---|---|---|---|
| `VICTORY_DRED=1` | off | no (before the device) | Device-removed data in any build: auto-breadcrumbs, breadcrumb contexts and page faults (`ID3D12DeviceRemovedExtendedDataSettings1`, else the first version). Our objects get names: lists `victory list #N`, `victory prelude or pass #N`, the queues, `victory tile heap #N`, title buffers and textures (as diagnostics builds did). In release builds the pipeline's name is set as a marker on a list each time it changes. | info once at start; at a loss: see below |
| `VICTORY_GPU_WATCHDOG=<ms>` | 2000 (0 off) | yes, `gpu_watchdog` | Every submission to the direct queue and every copy-queue job is noted (fence value, lists, their pipelines, the waits it carries). A thread looks every 100 ms; one still on the card past the limit is named once, and again when it finishes. Logging only. | warn per submission, per-frame line, part of the loss report |
| `VICTORY_GPU_PROGRESS=1` | off | no | Progress marks: before each list of a submission and after the last, a list of ours with two `WriteBufferImmediate` (`MARKER_IN`, `MARKER_OUT`) into a readback buffer, from a ring of 1,024 lists recorded once. The watchdog then names the list the card stands in. | in the watchdog's lines |
| `VICTORY_SWEEP_QUARANTINE_MS=<ms>` | 0 (off) | yes, `sweep_quarantine_ms` | What the sweep lets go of (windows of title memory, host copies of textures the title fills and so their tiles) is dropped no sooner than that, and only once every submission started by then has finished. | info once, per-frame line, part of the loss report |
| `VICTORY_MESH_GUARD=1` | off | no (read once; the stage is compiled once) | An indirect mesh record of more than 65,535 groups runs empty instead of being cut to 65,535: in the amplification stage (`VICTORY_INDIRECT_AMPLIFY=1`, which has no work budget) and in the clamp pass (every mesh record is clamped as one ahead of its draw, which already emptied such records). With the amplification stage its pipelines get new pipeline-library names: the first run makes them again. | info once |
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
INFO VICTORY_MESH_GUARD=1: an indirect mesh record of more than 65,535 groups runs empty (clamp pass and amplification stage)
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
  65,535 groups a draw, the clamp pass cut such records to 65,535 unless
  they were clamped ahead: `VICTORY_MESH_GUARD=1`.

## Recommended for the two test runs

`VICTORY_DRED=1 VICTORY_GPU_PROGRESS=1 VICTORY_DRIVER_WATCH=1` (the
watchdog is on by default), **`VICTORY_MESH_GUARD=1`**, and
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
- `VICTORY_MESH_GUARD=1`: a genuine record over 65,535 groups would draw
  nothing instead of part of it (no console dispatch runs more).
- The watchdog's report reads lists' pipeline names at submission (a copy
  of at most 24 names a list).

## Review (second pass, branch `fix/gpu-hang-review`)

A skeptical review of the three commits above, with the game not run.
What it found, and what changed:

- **The amplifier's guard could not be seen working.** With the play
  settings every one-record indirect mesh draw goes through the
  amplification stage (the clamp pass's line said "the largest group count
  asked for 0" in all 286 frame lines of the gameplay log), and the stage
  has nowhere to count what it empties. Fixed with a new per-frame line,
  only while draws go through the stage: one amplified draw in 16 has its
  record's x read on the CPU when it is recorded (title memory as it is
  then, through `host::read_into` as `VICTORY_INDIRECT_CPU` reads it; the
  refresh at submission reads the same bytes later), and those over
  65,535 are counted, apart for pages the CPU writes and pages shaders
  write (as of their last landing):

  ```
  INFO amplified mesh records sampled on the CPU when drawn (1 in 16), per frame: 92.4 read, of them 0.00 over 65,535 groups in pages the CPU writes and 0.31 in pages shaders write; such records run empty (VICTORY_MESH_GUARD)
  ```

  Its cost, with `VICTORY_INDIRECT_AMPLIFY=1` only: one 4-byte read of
  title memory per 16 amplified draws (~90 a frame at ~1,450 amplified
  draws a frame), a fraction of a millisecond on the recording thread.
  A non-zero "over 65,535" count, above all in the frames before a loss,
  is the hypothesis seen; all zeros in a run that still loses the card
  argue against it (a record the culling pass writes on the card after the
  sample is not seen, so zeros do not rule it out).
- **`VICTORY_INDIRECT_CPU=1` (experiment) cut such records to 65,535.**
  Now it empties them too with `VICTORY_MESH_GUARD=1`, as the clamp pass
  and the stage do. (Not in the play settings.)
- **The driver's entries could be missing from the report.** The driver
  wrote its first entry at 05.220 and the "MMU Fault" details at 05.712;
  a loss found sooner than half a second after the fault would have shown
  only "Error occurred on GPUID". The "when the host device went" report
  now reads the system's event log one second after the rest of the
  report is taken (once, at the loss), and says so ("read 1000 ms after
  the rest of this").
- **The progress marks' reuse rule had no test.** It is now a pure
  function (`watchdog::ring_free`) with a test: a mark list is never sent
  again before the fence passed the submission that last sent it, across
  the ring's end too. The rule itself was right.
- **The watchdog kept noting submissions if its thread could not be
  made** (nothing would have taken them out again). It now stops noting.
- **The copy queue's waits were put into words for every job even with
  the watchdog off** (`VICTORY_GPU_WATCHDOG=0`). Now only while it is on.

Looked at and left as they are:

- *GPU safety of the progress marks.* The mark lists are direct lists of
  two `WriteBufferImmediate` into a readback buffer (always in the
  `COPY_DEST` state, as `WriteBufferImmediate` asks), recorded once from
  their own allocator that is never reset; a list is sent again only once
  the direct fence passed the value of the submission that last sent it
  (`*submitted + 1` is the value signalled after that same submission, in
  both the sent-ahead and the waited paths); the marks are only taken when
  the submission is one `ExecuteCommandLists` (not with each list timed on
  its own); a submission of more than 511 lists goes without. The CPU reads
  the readback buffer (write-back memory, not write-combined) with volatile
  dword reads. Risk left: a driver may implement `MARKER_OUT` as a wait for
  idle; with ~43 lists a frame against the ~5,000 waits for idle the frame
  already has, it should not show, but it is one more reason to leave
  `VICTORY_GPU_PROGRESS` off for fps measurements.
- *The quarantine.* What it holds only adjusts counters and gives tiles
  back when dropped (`Drop for Shadow`, `HostTexture`, `Tiled`): nothing
  is unregistered by address, so dropping late cannot remove the entry of
  a newer copy. Its release rule (due time and every submission started by
  then finished) is the same as `retire`'s plus the time.
- *The mesh guard in the clamp pass* only sets the "ahead" bit for mesh
  records (kinds 3 and up); the clamp shader's "ahead" branch already
  emptied records over 65,535, and the WARP test of windows clamped ahead
  covers that branch.
- *DRED in release* names objects and sets a marker only with
  `VICTORY_DRED=1`; every DRED output read at a loss is walked by count and
  checked for null.
- *The watchdog's cost on by default*: about 43 lists a frame (gc3a run),
  each a serial and at most 24 pipeline names copied: well under a
  millisecond a frame on the submitting thread.

On the evidence:

- **The sweep before the loss came after the driver's first entry.** In
  the gameplay log the report is at 05.906 and its trail puts the last two
  sweeps 695.9 and 674.3 ms before it: about 05.210 (one window of 4 MiB)
  and 05.232 (113 texture copies), against the driver's first entry at
  05.220. The 113 textures went after the driver had already written its
  error, so they cannot be the cause; and a lost device's fences read all
  ones, which lets a settle (and so a sweep) through. The use-after-free
  reading is weaker than the table above says; the quarantine is still a
  cheap A/B.
- **The amplifier reading (55%)** rests on three losses that share the
  setting, but most runs that day had `VICTORY_INDIRECT_AMPLIFY=1`. Before
  the stage, such records went through the clamp pass, which empties them
  in windows clamped ahead and charges the others to a budget (at most
  about 16 records of 65,535 groups a list). The new sampling line is
  what can confirm or refute it from the log.

No upstream PR was ported here; the waits audited come from fxceless's
upstream and tibzejoker's PR #7 (`VICTORY_ASYNC_SUBMIT=2`, `JOIN_*`,
`ASYNC_PRESENT`). DRED and `WriteBufferImmediate` markers follow
Microsoft's documentation of device-removed extended data.
