# Environment switches

VIctory reads about 575 `VICTORY_*` environment switches. **Each one is
documented in a comment where it is read** — that comment is the source of
truth for its exact default and effect. This page is a curated index of the
switches you are most likely to set; it is not exhaustive. To find a switch's
definition: `grep -rn VICTORY_THE_NAME crates/ apps/`.

Switches take precedence over `victory.toml`. Most behaviour switches are
**off (`0`/unset) by default**; a run is "as before" with none set. Boolean
switches are read as `== "1"` unless the comment says otherwise.

## Paths and configuration

| Switch | Effect |
|---|---|
| `VICTORY_GAME_DIR` | Your own dump of the title (the folder with the exe and the `.rpf` archives). |
| `VICTORY_CACHE_DIR` | Derived data (translated shaders, pipeline library). Default `%LOCALAPPDATA%\victory\cache`. |
| `VICTORY_DATA_DIR` | Title saves and `crashes.log`. Default `%LOCALAPPDATA%\victory\data`. |
| `VICTORY_CONFIG` | Use a different config file than `victory.toml`. |
| `VICTORY_TEST_TITLES` | Other Xbox Series titles for platform tests, `;`-separated. |

## Running and diagnostics

| Switch | Effect |
|---|---|
| `VICTORY_LOG=<target>=level,...` | Log filter, e.g. `victory_loader=trace,info`. Release logs info+. |
| `VICTORY_CAPTURE=<dir>` | Save presented frames (powers of two, or `VICTORY_CAPTURE_FRAMES=<list>`). |
| `VICTORY_DUMP_TARGETS=1` | With a capture, also save every render target of those frames. |
| `VICTORY_D3D12_DEBUG=1` | Host D3D12 debug layer (diagnostics build). |
| `VICTORY_D3D12_GBV=1` | With the above, GPU-based validation (slow; reports bad descriptor use). |
| `VICTORY_D3D12_DRED=1` | On a GPU failure, names the allocation and the commands in flight. |
| `VICTORY_D3D12_WARP=1` | Host device on the software adapter (slow; cannot reset the driver). |
| `VICTORY_PANEL=1` | On-screen numbers panel (also F3). Costs a little. |
| `VICTORY_AB`, `VICTORY_LIVE_ALT` | A/B one switch inside a single run (runs vary ~2 fps). |
| `VICTORY_STACKS=20,60` | Every thread's stack at those seconds. |
| `VICTORY_WATCH=<rva>,...` | Log a title u32 when it changes. |
| `VICTORY_KEYS=30:Enter,35:X` | Press keys at those seconds. |
| `VICTORY_CRASH_SAVE=<rva>:<len>,...` | Save title memory to the cache folder on a crash. |
| `VICTORY_SKIP_PIPELINES=<hash>,...` | Omit draws/dispatches whose pipeline contains a listed shader hash. |
| `VICTORY_GPU_TIMING=1` | Time each list of a submission; log slow lists with their slowest commands. |
| `VICTORY_PIPELINE_WAIT_MS=24` | Lower the per-frame wait for background pipeline creation (default 500 ms). |
| `VICTORY_SHADER_LOOP_LIMIT=<n>` | Cap translated-shader loop iterations per lane (diagnostic). |
| `VICTORY_DESC_WHY=1` | Print why a descriptor read stops a shader (e.g. the shadow-mask traces). |
| `VICTORY_MEMORY_MAP=1` | Log the private-memory breakdown (allocators, windows, uploads). |

## Rendering — ray tracing and shadows (see `docs/fork-ray-tracing.md`)

| Switch | Default | Effect |
|---|---|---|
| `VICTORY_BVH_BUILDER=1` | off | Run the title's acceleration-structure builder stages (rooms get indirect light). |
| `VICTORY_AMBIENT=<share>` | off | Light estimate where rays find nothing (`1` all of it; `constant:<level>`). Off while the builder runs. |
| `VICTORY_AMBIENT_SHADE=<strength>,<m>` | 1,0.6 | The crease shade of the ambient estimate. |
| `VICTORY_AMBIENT_AIR=<multiple>` | off | Bound air-scatter by a multiple of surrounding light (clears the day-room haze). |
| `VICTORY_SHADOW_ADDR=1` | off | **(ultimate3)** Emit the heap-indexed view-address path for the shadow-mask traces. Needs the host half (see `docs/shadow-mask-fix.md`); shader half only so far. |
| `VICTORY_BVH_NO_HIT` | — | Node tests read nothing, so every ray misses (diagnostic). |
| `VICTORY_BVH_SIZES_WAIT=1` | off | Wait for submissions that ask structures' sizes (does not fix overlap; costs ~20 ms). |
| `VICTORY_BVH_QUARANTINE=1` | off | The builder's stores do not reach title memory (first-test safety). |
| `VICTORY_BVH_BOUNDS=0` | on | Turn off bounding of the builder's accesses. |
| `VICTORY_BVH_FENCES=0` | on | No fences around the builder's buffer atomics. |
| `VICTORY_BVH_TRACE=1` | off | Log sizes asked, when they came, and the copies. |

## Performance (see `docs/findings.md`, `ADDENDUM-PERF-PIPELINE.md`)

Most of these are measured gains kept **off by default** pending a safe-default
pass. Promote one only after an A/B in one run and a visual capture.

| Switch | Default | Effect |
|---|---|---|
| `VICTORY_LATE_WAIT=1..3` | off | Send submissions ahead of the wait for the card (CPU/GPU overlap). |
| `VICTORY_RELAY=1` | varies | Land shader-written results on a relay thread as the card finishes. Can cause black streaks on clothes under load. |
| `VICTORY_RELAY_COPY=1` | off | Relay fetches on the copy queue, not behind draw work. |
| `VICTORY_THIN=1` | varies | Record and translate list calls on eight threads. |
| `VICTORY_ASYNC_SUBMIT` | — | Drain submissions on a dedicated thread. |
| `VICTORY_CARD_BUFFERS_MIN_MB`, `VICTORY_CARD_BUFFERS_MB` | auto | Large title buffers get a card copy; shader writes found by hashing on the card. |
| `VICTORY_INPLACE=1` | off | Open title memory as a graphics heap so buffers are used in place (pins pages; probe-gated). |
| `VICTORY_WINDOW_KB=256` | 4 MiB | Finer window grid for title-memory windows (saves memory). |
| `VICTORY_WINDOW_TAILS` | off | Windows start at the ask, not the 4 MiB line below. |
| `VICTORY_HEAP_SYNC_FAST`, `VICTORY_WIDE_SYNC` | off | Batched descriptor retirement; one cross-heap sync pass. |
| `VICTORY_WATCH_FAST` | off | Answer write-watch range asks from a flat table, not a BTreeMap walk. |
| `VICTORY_STORE_BESIDE`, `VICTORY_COPY_BESIDE` | off | Perform a faulting title store/memcpy through an unguarded view without reopening the block. |
| `VICTORY_INDIRECT_REST=0` | 0 | Keep an oversized indirect record from blanking later records (leave at 0). |
| `VICTORY_DYNAMIC_SLOTS=<x>` | 1 | Scale the dynamic descriptor-slot pool (up to 4×) to drop fewer draws. |
| `VICTORY_SECTION_MB=2` | — | Title physical pages in 2 MiB sections. |
| `VICTORY_SHADER_OPT=0` | on | Turn off the translated-shader clean-up passes (~43% fewer host instructions). |

**Leave off** (correctness/latency trades, not free wins): `VICTORY_CARD_ALL`,
`VICTORY_INPLACE` (beyond probing), `VICTORY_CARD_BUFFERS_MIN_KB` lowered.

## Shaders (translation)

| Switch | Default | Effect |
|---|---|---|
| `VICTORY_WAVE_WIDE=1` | off | Wave-wide (64-lane) barrier form; TDR risk via loop widening — keep off. |
| `VICTORY_WAVE_PAIRS`, `VICTORY_WAVE_PAIRS_ONLY`, `VICTORY_WAVE_PAIRS_FP32` | off | Paired-lane Wave64→Wave32 forms (from forks). |
| `VICTORY_TRANSLATOR` | — | Translator-cache control; verify `made=0` on a re-run so the cache is not invalidated for nothing. |
| `VICTORY_EMIT_DUMP` | off | Dump emitted modules (diagnostic). |

## Audio

| Switch | Effect |
|---|---|
| `VICTORY_AUDIO_CAPTURE=<file.wav>` | Record what the title plays (our process only). |
| `VICTORY_XMA_DUMP=<dir>` | Save the first 32 XMA voices' context and input. |

## NVIDIA (DLSS / Streamline, the NVIDIA line)

| Switch | Effect |
|---|---|
| `VICTORY_DLSS`, `VICTORY_DLSS_MODE` | DLSS Super Resolution and its mode. |
| `VICTORY_FRAMEGEN`, `VICTORY_FRAMEGEN_X` | DLSS Frame Generation and its factor (2×–6×). |
| `VICTORY_STREAMLINE_DIR` | Where NVIDIA's Streamline files are (fetched separately; not in the repo). |
| `VICTORY_NATIVE_1440P` | Native 1440p path (Series X profile). |

> This index is maintained by hand and can drift. The comment at each switch's
> read site is authoritative; grep for the name to find it.
