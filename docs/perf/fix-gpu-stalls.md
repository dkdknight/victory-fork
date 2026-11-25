# Long stalls while the world streams in (branch `fix/gpu-stalls`)

The frames of 0.1-2 fps (570-6,800 ms) with the card 1-18% busy, in the
cutscene runs of 2026-11-25 (`gc2`, `gc3a`) and when gameplay loads
(`us.j`, 0.2 fps at 21:02). Written with the help of an AI assistant
(Claude Code); every number is from those logs. Not measured in a run of
this branch yet.

## What the logs say

Every such frame line has its time in "preparing ... textures N ms", and
the line "texture refreshes per frame" says what those milliseconds are:

| run | frame | textures | of it waiting for title memory to hold still |
|---|---|---|---|
| gc2 | 20:24:09, 0.1 fps, 6,785 ms | 4,689 ms | 4,565 ms |
| gc2 | 20:24:34, 0.2 fps, 6,482 ms | 4,949 ms | 4,795 ms |
| gc3a (warm cache) | 20:37:35, 0.3 fps, 2,878 ms | 2,204 ms | 1,997 ms |
| us.j | 21:02:12, 0.2 fps, 4,073 ms | 2,682 ms | 2,156 ms |
| us.j | 21:02:22 | 2,655 ms | 2,291 ms |

"Waiting for title memory to hold still" is the wait of a texture refresh
for the lock the runtime takes alone while it changes what title memory
shows (`crate::watch::title_memory_reading` / `title_memory_remapping`,
taken by `victory-hle`'s `XMemMapPhysicalPages`). The standard library's
lock lets no new reader in while a writer waits, so:

- each change of mapping waits for every read in place under way, and a
  read in place holds the lock while the texture is hashed and detiled
  whole (milliseconds for a big texture);
- every refresh asked for meanwhile waits behind that change;
- `XMemMapPhysicalPagesScatter` (one page at each of many addresses) takes
  the lock once **per page**: a scatter of hundreds of pages is hundreds of
  such convoys.

The submitting thread is the one that waits (the card's clock line: "1,371
of 3,040 ms while the submitting thread was working (the card idle behind
us)").

The other hypotheses, checked:

- **Shader translation / pipeline creation.** Already on workers
  (`pipeline/make.rs`, decision 0007). A draw's first use waits for its
  pipeline within a budget of 500 ms per frame (`VICTORY_PIPELINE_WAIT_MS`),
  then is dropped. In a warm run (`gc3a`, every shader from the cache) the
  slow frames show "0 being made": not the cause there. With a cold cache
  (`us.j`: `hits=0 misses=2898`, the first run of this build on play-lod's
  cache `victory-lod-data`; `gc2`/`gc3a` used `victory-gc1-data`) the first
  minutes have 1,000-1,800 pipelines queued and frames that can take the
  whole half second. Until now nothing in the log said how much of a frame
  was that wait.
- **The library saved under the mapping.** Saves serialise the whole 727 MB
  library on a pipeline worker under a lock every store takes: a worker
  that compiled a pipeline cannot hand it out until the save is done, and a
  draw waiting for it waits with it. Four saves in `us.j`, none timed.
- **Every run compiles the same ~255 pipelines again** (`made=253-278` in
  every run against 3,000+ loaded): the library refuses them under their
  name (load E_INVALIDARG, e.g. `[mesh 6a84008fd80c7e59, pixel
  64e3fe8556c88111] in 384.2 ms (the library did not load it)`), and the
  store after the compile is refused too (the name is there). Each run pays
  them again while loading.
- **Texture uploads "waiting for their memory"** and **the copy-back**: no
  wait of the submitting thread in those lines.

The warm cache works as designed: the early list (`early-pipelines.txt`)
starts every title pipeline an earlier run drew with at the title's
creation of it, during loading (gc3a: 1,075 loaded 1.3 s after start,
3,183 at 34 s). It cannot start before the title creates the pipeline (the
key is its shader containers and state). What a warm-up run needs: the
**same build and the same translator switches** as the test (the shader
cache is keyed on the translator's source and switches), and the same
cache folder (`VICTORY_CONFIG`).

## Changes

All off by default except the counters, which only add log lines.

| Switch | Live key | What it does |
|---|---|---|
| `VICTORY_TEXTURE_NO_WAIT=1` (existed) | `texture_no_wait=1/0` (new) | a texture refresh that finds the remap lock taken reads through the kernel instead of waiting; a texture with a page missing is read at its next refresh. On by default upstream (fffluffy-dev, branch `integ/textures2`). |
| `VICTORY_REMAP_BATCH=1` (new) | no (read once) | `XMemMapPhysicalPagesScatter` maps all its pages under one take of the page table's lock and the remap lock, not one take per page. Same pages, same order, same result. |
| `VICTORY_TEXTURE_SHORT_HOLD=1` (new) | `texture_short_hold=1/0` | a texture whose memory is all mapped is copied out under the lock, the lock let go, then hashed and detiled. One extra copy per refresh that reads in place. |
| `VICTORY_PIPELINE_WAIT_EACH_MS=n` (new) | `pipeline_wait_each_ms=n` | one first use waits at most `n` ms for its pipeline (its draw is dropped past it, as past the frame's budget); the rest of the budget stays for the cheap ones (library loads take ms). |
| `VICTORY_PIPELINE_WAIT_MS=n` (existed) | `pipeline_wait_ms=n` (new) | the frame's budget, now changeable live. |
| `VICTORY_PIPELINE_WORKERS=n` (new) | no | pipeline workers (default half the logical processors, 2-8). |
| `VICTORY_LIBRARY_SAVE_ASIDE=1` (new) | no | library saves on a thread of their own (`victory-library-save`, below normal priority); a pipeline made during a save is handed out at once and stored after it. |
| `VICTORY_LIBRARY_ALT_NAME=1` (new) | no | a pipeline the library refuses under its name is stored, and looked up, under a second name (`<name>~2`): the ~255 compiled again by every run are loaded from the second run on. |

## Log lines (one each per frame-rate line, when there is something to say)

```
title memory remaps per frame (the runtime changing what it shows): 41.0 changes, 380.2 ms holding the lock, 352.9 ms of that waiting for reads in place to end; 3.0 calls changed 412.0 pages under one take (VICTORY_REMAP_BATCH=1: on); reads in place 260 per frame holding the lock 401.7 ms, the longest 38.4 ms; 112.0 reads found it taken and did not wait
texture reads in place that let the runtime remap before hashing and detiling, per frame (VICTORY_TEXTURE_SHORT_HOLD=1, live texture_short_hold): 180.0, 95.3 MB copied out under the lock
host pipelines per frame: 12.0 made by the workers in 640.0 ms of their time (10.0 loaded from the library, 2.0 compiled), 0.0 made on other threads in 0.0 ms; shaders translated 4.0 on the workers in 210.0 ms, 0.0 on other threads in 0.0 ms; first uses waited 6.0 times 92.0 ms (title threads 92.0 ms, our submit thread 0.0 ms, other threads of ours 0.0 ms), the longest 33.0 ms, 4.0 got their pipeline and 2.0 not (their draws dropped), 0.0 found the budget spent; budget 500 ms a frame (VICTORY_PIPELINE_WAIT_MS, live pipeline_wait_ms), 33 ms each (VICTORY_PIPELINE_WAIT_EACH_MS, live pipeline_wait_each_ms); 431 queued now, 3 of them asked for by draws
host pipeline library since the line before: 1 saves, 2410.0 ms serialising and 3.1 ms renaming in all, the longest save 2415.2 ms (VICTORY_LIBRARY_SAVE_ASIDE=1: saves on a thread of their own, stores not held); 5 pipelines handed out during a save and stored after it; 0 stores refused (the name was there with another description: compiled again every run), 9 loaded and 0 stored under a second name (VICTORY_LIBRARY_ALT_NAME=1: on); per frame 0.25 saves
```

(The numbers above show the form, not a run.) Existing lines that go with
them: "texture refreshes per frame, all threads: ... ms waiting for title
memory to hold still", "texture refreshes that did not wait for title
memory to hold still, per frame: N read through the kernel, N left for the
next refresh", and the frame line's "dropped N without a host pipeline (N
being made)". The "host pipeline library saved" line now also gives
`serialise_ms`, `rename_ms` and the thread. When the host device goes, its
report gains "host pipelines: N queued, N of them asked for by draws".

How to read them in the final run:

- **The fix works** if "waiting for title memory to hold still" stays
  near 0 (with `TEXTURE_NO_WAIT`) and "read through the kernel" carries
  the refreshes instead; "left for the next refresh" should stay small
  (each is a texture a frame late).
- **The runtime's own stalls**: "ms of that waiting for reads in place to
  end" is time the title's streaming thread stood still; it should drop
  with `REMAP_BATCH` and `TEXTURE_SHORT_HOLD` ("the longest" read hold
  shows the worst one).
- **Pipelines**: "first uses waited ... ms" is the frame's own wait for
  pipelines; with a warm cache it should be a few ms, "compiled" near 0
  from the second run with `ALT_NAME`; "stores refused" says how many are
  still compiled every run.

## Recommended for the global test

```
VICTORY_TEXTURE_NO_WAIT=1
VICTORY_REMAP_BATCH=1
VICTORY_TEXTURE_SHORT_HOLD=1
VICTORY_PIPELINE_WAIT_EACH_MS=50
VICTORY_LIBRARY_SAVE_ASIDE=1
VICTORY_LIBRARY_ALT_NAME=1
```

Warm-up run first with exactly these and the same build and cache folder,
then the real run. `VICTORY_PIPELINE_WAIT_MS` stays at 500 (passes the
title runs once still get their pipeline when it is cheap).

## Risks

- `TEXTURE_NO_WAIT`: a texture that arrives during a remap shows a frame
  later; a read through the kernel costs ~0.6 ms/MB against 0.3 in place
  (with `VICTORY_COPY_PLAIN=1` the read is a plain copy with faults caught).
- `TEXTURE_SHORT_HOLD`: the levels-on-demand judgement of which levels are
  held looks at the mapping after the lock is let go; if a page goes in
  between, fewer levels are held for that refresh (never a level of
  zeros). Partly mapped textures keep the lock to the end for that reason.
- `REMAP_BATCH`: the runtime holds the lock for the whole scatter: one
  longer hold instead of many short ones. A read in place that comes during
  it waits longer once (with `TEXTURE_NO_WAIT` it does not wait).
- `PIPELINE_WAIT_EACH_MS`: a pass the title runs only once and whose
  pipeline takes longer than the cap is lost (as past the frame's budget
  today). Warm runs load such pipelines from the library in milliseconds.
- `LIBRARY_SAVE_ASIDE`: a pipeline made during a save goes into the next
  save; a run that ends right after loses it (made again next run).
- `LIBRARY_ALT_NAME`: one more library lookup for each pipeline not in the
  library (a few ms while loading); the library grows by the ~255 entries.

## Tests

- `cargo test -p victory-gpu --lib` (RTX 5080): 194 passed. New:
  `make::tests::a_cap_per_pipeline_bounds_one_wait`,
  `make::tests::threads_are_told_apart_by_name`,
  `library::tests::the_second_name_differs_from_every_first_name`,
  `library::tests::stores_refused_or_during_a_save_are_kept` (a refused
  store is counted, a store put aside is stored by `store_aside`).
- `cargo test -p victory-hle --lib xmem`: 13 passed, among them the new
  `a_batched_scatter_maps_as_the_scatter_does` (same mapping as the page
  by page scatter, a reader in place beside 500 batched remaps never
  faults, page 0 and null pages unmap).
- WARP (`VICTORY_D3D12_WARP=1`, `--features diagnostics`): with one test
  thread 209 passed and `depth_bounds_pipeline_loads_and_filters_draws`
  fails (WARP has no depth bounds test); with several threads the process
  ends in an access violation. Both are the same on
  `feature/levels-on-demand` without this branch.
- `cargo build --release -p victory`, also `--features diagnostics`.

## Credits

- `VICTORY_TEXTURE_NO_WAIT` and the kernel read it uses come from upstream
  VIctory (fffluffy-dev, commit 2b1c205 on `integ/textures2`, which makes it
  the default).
- `VICTORY_PIPELINE_WAIT_MS` and the shared budget are upstream's.
