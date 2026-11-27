# Frame-parity / frame-age structures in game_scarlett_master.exe (1.0.2.1)

Static analysis only (capstone and the Ghidra project); nothing was run. All addresses are offsets from the image base 0x140000000.
Frame counter: `+0xA9437AC` (dword, incremented once per frame at `+0x24747C0` inside the frame limiter `FUN_142474740`).
Scratch scripts: `C:\Users\Knight-HD\ghidra_explore\agent_parity\` (`lib.py` has a numpy rip-relative xref scanner; `sites.txt` traces all 141 direct reads).

## 0. Summary

* I found **141 direct rip-relative reads/writes** of the counter. Only **12 of them compute `frame & 1`** (or `test [frame],1`). One more parity site reads the frame through a register copy (`+0x2484193` -> `+0x2484204`). There is also **one `frame % 3` site** (`+0x2484470`). The other sites are stamps, equality checks, or age comparisons.
* Parity structures (2 slots) that hold or protect memory or objects:
  * **A** `+0x8759F70` (2 x 16 B): deferred release of a (resource, view) pair.
  * **B1..B4** (2 x 0x100-byte bitmaps each): "slot used this frame" bitmaps that protect eviction from four keyed object caches that own slots in GPU buffers.
  * **P** `+0x75E7640` (2 x 0x80 B): GPU-profiler table of timestamp-query indices per frame. It is diagnostic only.
  * **L** `+0x75DDB60` (2 x 8 B): the frame limiter's own fence ring. You already patch it. It is listed so the inventory is complete.
* **Key timing result (section 4).** Because the counter increments at the end of the frame and the bitmap clear lags the check, B1..B4 and A release things 3 frames after their last use. They are therefore **still safe with 3 frames in flight** (CPU F plus GPU F-1 and F-2), just like the 3-version renamed buffers. Widening them to `&3` (section 1) is only needed for 4 or more in flight. The structures that really break at 3 in flight are those with a **2-frame window**.
* Frame-age gates with a window of 2 frames are just as dangerous at 3 frames in flight. The main ones are the **texture-streaming / defrag release queue** (`release frame = frame + 2`, `FUN_140121f20` / `FUN_1443c50a0` -> `FUN_144fd22d0`), a dead-entry compactor (`FUN_1425daae0`), a GPU readback after 2 frames (`FUN_142a65a80`), and several "wait 2 frames" state machines.
* **The command-block pool itself is not parity based.** Blocks (list head `+0x75DF1D8`, lock `+0x75DF198`) are pushed back by `FUN_1424749d0` with a real queue fence value (`bzhi(counter,56)`, queue id in the top byte). `FUN_14005c280` only reuses a block when `FUN_144dad000(fence)` reports it complete; `-1` means never submitted, `-2` means skip. The only frame data in the pool is `frame & 0xFF` equality stamps (`+0x75DF1E8[queue]`, block+0x50, block+0x28). Those stamps are safe at any depth. The linear-allocator pages at `+0x75DF220/230/240` are recycled by `FUN_144e9b530` using a per-page counter, not by frame parity. So the overwrite has to come from something that frees or reuses memory or objects on a **frame-count** basis while that memory or object is still referenced by an in-flight block. The ranking is in section 5.
* Dynamic "renamed" buffers (the ~45 `shl eax,4; xor; test eax,0x7FFFFFF0; lock cmpxchg [obj+0x50]` sites) already have **3 versions**. `FUN_144f244d0` returns 3, or 1 for static buffers. They are exactly safe at 3 in flight and unsafe at 4. They are not widened here; see section 3.

## 1. Parity sites (`& 1`), exact bytes and how to widen

The new areas must be ours, zero-filled, and reachable with rip+disp32 (within +-2 GB of the exe, for example a section or VirtualAlloc near the image). Otherwise use a code cave with `mov r64, imm64`.

### A: `+0x8759F70`, 2 slots x 16 B (slot = {+0 resource, +8 object}), relocate to 64 B, 16-byte aligned
| site | bytes | instruction | role | patch |
|---|---|---|---|---|
| 6991E5 | 8b05c1a52a0a | mov eax,[frame] | FUN_1406991e0 (called first from FUN_1406991c0 each frame): releases slot[frame&1] | - |
| 6991EB | 488d0d7e0d0c08 | lea rcx,[+8759F70] | slot base | new disp32 -> NEW_A |
| 6991F2 | 83e001 | and eax,1 | parity | `83 e0 03` |
| 6991F5 | c1e004 | shl eax,4 | 16-B slots | keep |
| 6991F8/6991FC | 488d3401 / 488d4c0808 | lea rsi,[rcx+rax] / lea rcx,[rax+rcx+8] | `FUN_140699220(slot+8)` -> `FUN_14529c130` (refcount at +0x10, release object); `FUN_140699250(slot+0)` -> `FUN_144fb0380` (graphics resource Release, refcount word at +0x46) | keep |
| 33A3EE8 | 8b05bef85907 | mov eax,[frame] | FUN_1433a3ee0: moves the pending pair `+0x8759F00/08` into slot[frame&1], then zeroes the pending pair | - |
| 33A3EEE | 488d0d7b603b05 | lea rcx,[+8759F70] | | new disp32 -> NEW_A |
| 33A3EF5 | 83e001 | and eax,1 | | `83 e0 03` |
| 33A3EFB | c5f8290408 | vmovaps [rax+rcx],xmm0 | **aligned store**: NEW_A must be 16-byte aligned | keep |
| 5B350BD | c5fc1105ab4ec202 | vmovups ymmword [+8759F70],ymm0 | static init (zeroes 32 B once at startup) | optional: point at NEW_A; our area is already zero |

Callers of FUN_1433a3ee0: FUN_140699280 (render, runs right after the release), FUN_1433a3c20, FUN_1433a6910 (creates the pair). Order each frame: release slot[f], then store the new pair into slot[f]. This gives 2 frames of delay now and 4 after the patch. `+0x8759F80..` is **not** part of A: `+0x8759F90..` is a separate pointer list (count `+0x8759FA0`, used by FUN_140dc0ec0 / FUN_1433a3b60). That is why A must be relocated and cannot grow in place.

### B1..B4 bitmaps (layout per object: +0 u16 bind slot, +8 GPU buffer, +0x10/+0x18 ..., **+0x20 + parity*0x100 bitmap (2048 bits)**, +0x220..+0x27F hash map and lock)
| bitmap | base | bind-slot hash, GPU buffer (FUN_140ad0ad0 1st arg), init fn | key -> bit index |
|---|---|---|---|
| B1 | 8D44B68 | FUN_1408e8930(0x88218418), 0x12C000, FUN_145741c60 | id / 3840 (`FUN_144e2f8f0`: *0x88888889 >> 43) |
| B2 | 8D44DE8 | FUN_1408e8930(0x127F3D17), 0x9E200, FUN_1457d83c0 | `FUN_144e8a000(id)` |
| B3 | 8D45068 | FUN_1408e8930(0xE0495228), 0x6A4, FUN_1457d0010 | id / 136 (`FUN_144e2f900`: *0xF0F0F0F1 >> 39) |
| B4 | 8D452E8 | none (slot 0), 0x6A4, FUN_145bd7790 | [obj+0xC] * 0xF0F0F0F1 >> 54 (in FUN_1435e4790) |

Widening: give each bitmap its own 4 x 0x100 = 0x400-byte area (NEW_Bn), and point every `lea` at **NEW_Bn - 0x20** so the existing `+0x20` displacements still work. Do not move the header or the hash-map fields.

| site | bytes | instruction | role | patch |
|---|---|---|---|---|
| **setters** (`lock or [r8+rcx*8+0x20], 1<<idx`) | | | | |
| 4E2F91B / 4E2F92A | 4c8d054652f103 / 83e001 | lea r8,[B1] / and eax,1 | FUN_144e2f910 (B1). Called from FUN_144e2f880(entry): marks the entry's main object in B1 and each sub-object in B3 | disp -> NEW_B1-0x20; `83 e0 03` |
| 4E2F94B / 4E2F95A | 4c8d051657f103 / 83e001 | lea r8,[B3] / and eax,1 | FUN_144e2f940 (B3) | disp -> NEW_B3-0x20; `83 e0 03` |
| 4E8A01B / 4E8A02A | 4c8d05c6adeb03 / 83e001 | lea r8,[B2] / and eax,1 | FUN_144e8a010 (B2). Called from FUN_144e89f90 (B2 + B3 for the sub-objects) | disp -> NEW_B2-0x20; `83 e0 03` |
| 35E47DB / 35E47EA | 4c8d05060b7605 / 83e001 | lea r8,[B4] / and eax,1 | FUN_1435e4790 (B4; vtable method, pointer at +0x6568FD0 table via FUN_140b833e0) | disp -> NEW_B4-0x20; `83 e0 03` |
| **per-frame clear** (32 x `xchg [rax+rcx+0x20..0x118], 0`) | | | | |
| 1167F91 / 1167F9D | 488d0dd0cbbd07 / 83e001 | lea rcx,[B1] / and eax,1 | FUN_141167e20 | disp -> NEW_B1-0x20; `83 e0 03` |
| 11676A1 / 11676AD | 488d0d40d7bd07 / 83e001 | lea rcx,[B2] / and eax,1 | FUN_141167530 | disp -> NEW_B2-0x20; `83 e0 03` |
| 11496E1 / 11496ED | 488d0d80b9bf07 / 83e001 | lea rcx,[B3] / and eax,1 | FUN_141149570 | disp -> NEW_B3-0x20; `83 e0 03` |
| 112E1CB / 112E1D7 | 488d0d1671c107 / 83e001 | lea rcx,[B4] / and eax,1 | FUN_14112e060 | disp -> NEW_B4-0x20; `83 e0 03` |
| **readers** (parity passed explicitly) | | | | |
| 4E2994 | 4c8d05cd218608 | lea r8,[B1] (FUN_1404e2990(idx, slot): `shl rdx,8`; `[r8+rax*8+0x20]`; `bt`) | the slot argument comes **only** from the wrapper FUN_1404e2960, which calls slot 0 (`xor edx,edx`) and then tail-calls slot 1 (`mov edx,1`) = "used in either frame" | rewrite wrapper+leaf, see below |
| CDCFF4 | 4c8d05ed7d0608 | lea r8,[B2] (FUN_140cdcff0) | wrapper FUN_140cdcfc0 | same |
| E81F64 | 4c8d05fd30ec07 | lea r8,[B3] (FUN_140e81f60) | wrapper FUN_140e81f30 | same |
| 35DE514 | 4c8d05cd6d7605 | lea r8,[B4] (FUN_1435de510) | wrapper FUN_1435de4e0 | same |

The readers need slots 0..3, so the two `edx=0/1` calls are not enough. Each wrapper+leaf pair is contiguous and the leaf has no other caller: 4E2960..4E29B1, CDCFC0..CDD011, E81F30..E81F81, 35DE4E0..35DE531, 0x52 bytes each. Overwrite the **wrapper** (its entry point is what the clear functions call) with this 48-byte body:
```
89 c8                      mov  eax, ecx
c1 e8 06                   shr  eax, 6
4c 8d 05 <rel32>           lea  r8, [rip + NEW_Bn]        ; slot 0, no -0x20 here
49 8b 14 c0                mov  rdx, [r8+rax*8]
49 0b 94 c0 00 01 00 00    or   rdx, [r8+rax*8+0x100]
49 0b 94 c0 00 02 00 00    or   rdx, [r8+rax*8+0x200]
49 0b 94 c0 00 03 00 00    or   rdx, [r8+rax*8+0x300]
48 0f a3 ca                bt   rdx, rcx
0f 92 c0                   setb al
c3                         ret
```
(It returns al = 1 if the bit is set in any of the 4 slots, which is the same contract as before. Leave the old leaf bytes or fill them with int3.) If you only patch the leaves' `lea` without rewriting the wrappers, the eviction will check just 2 of 4 slots and the widening is useless.

Header word (base+0) and buffer (+8): read by FUN_1451a5dc0 (binds the 3 buffers into TLS context `+0x12A8`, slot table at +0x40) and written by the 4 init functions. These are not parity data; leave them alone. `+0x8D44B60` is a separate bind slot (FUN_144e89ed0 / FUN_1435ddc40), also not parity.

### P: GPU-profiler query-index table `+0x75E7640`, 2 x 0x80 B
| site | bytes | instruction | role | patch |
|---|---|---|---|---|
| 2484193 | 8b3513f64b08 | mov esi,[frame] | FUN_142484180 (per frame, GPU frame-time) | - |
| 2484204..248422D | 89f0 / 488d0d33341605 / 83e001 / c1e007 / 4 x vmovdqu | eax=frame; lea rcx,[+75E7640]; and eax,1; shl 7; fill slot with 0xFF; cursor `+0x75E6EF0` = slot | each submit (FUN_1402a42d0 @2A42FF..2A4327) appends `(query_off-base)>>3` bytes at the cursor | lea -> NEW_P; `83 e0 03` |
| 2483E52/2483E55 | 83e101 / 4c8d05e4371605 | and ecx,1; lea r8,[+75E7640] | FUN_142483e50(frameId) looks up table[frameId&1] (frameId = max of 3 query results, FUN_142483de0) | `83 e1 03`; lea -> NEW_P |
Diagnostic only. The indices are bounded (byte index < 0x80), so this cannot overwrite memory.

### L: frame-limiter fence ring `+0x75DDB60`, 2 x 8 B (FUN_142474740)
`2474785 83e001` (store slot = submit&1), `247479B 83e001` (wait slot = waited&1), `2474794 83fd02` (cmp outstanding,2), `24747B3 83fd01`. If only the compares were raised, then with 3 outstanding `(waited&1) == (submit&1)` and the loop waits on the **newest** fence (a full stall, but safe). For a real 3- or 4-deep limiter the ring itself needs 4 slots (`and eax,3` at both sites plus a 32-byte area; `rbx` comes from `lea rbx,[rip+0x51693E0]` @2474779). FUN_140126880 (flush) calls the limiter `[+0xA9437B4]` times (the back-buffer count) with fence -1. With deeper queues it needs at least as many calls as the queue depth.

### Visual alternation (do not widen)
* `A746D3 f605d2f0ec0901 test byte [frame],1` (FUN_140a74560): selects jitter lane x or y (blendps 1/2), a checkerboard/TAA alternation.
* `25D01D7 f605ce35370801 test byte [frame],1` (FUN_1425d01b0): mode = (frame even ? 4 : 1), alternate-frame update.
* `503AF99..503AFA2 a803` `(frame + obj[0x10]) & 3`: staggered 1-in-4 update flag. Not storage.

## 2. Frame-age gates (window in frames) and patches

"safe@3" means the gate stays correct with 3 frames in flight. The patches target a 4-frame window to match `&3`.

| site | bytes | logic | what it decides | safe@3 | patch (to 4) |
|---|---|---|---|---|---|
| 122147 | 83c002 | stamp = frame+2 (0 if flag clear) -> batch+0x88 | FUN_140121f20: texture-streaming/defrag batch whose GPU copy fence (batch+0x58 vs FUN_1400065c0) has passed. Release of the old pages/resources (FUN_144fb0380, FUN_1443c4bf0) is delayed until frame >= stamp | **no** | `83 c0 04` |
| 43C5409 | 83c002 | same (FUN_1443c50a0, mip-residency change; commit via FUN_14221c0c0, 4 KB page counters) | same queue | **no** | `83 c0 04` |
| 4FD2321 / 4FD2434 | 8b0d85149705 3b8888000000 / 390d72139705 | `frame < batch+0x88` -> not yet; FUN_144fd22d0 (queue `+0x9C3D808/818/828`, lock `+0x9C3E040`) processes the releases | consumer of the two lines above | - | none |
| 2A65AF4 | 83c602 | deadline = frame+2 after FUN_142a69120 issues GPU work; `2A65AA0 390506dded07` reads the result (FUN_145cdf0d0) when frame >= deadline | GPU readback | no (stale data) | `83 c6 04` |
| F33642 | 83c502 | stamp frame+2 into `+0x8552260 + i*0x1400 + 0x10C + j + 0x2C` when the slot is -1 (FUN_140f32df0) | deadline; consumer not traced | ? | `83 c5 04` (low confidence) |
| 25DAB58 | 4183fa02 | `frame - e[+0xD0] >= 2` and e[+0xD8]==1 | FUN_1425daae0: compacts an array of 0x110-byte entries, destroys dead entries and **releases refcounted objects** they hold | **no** | `41 83 fa 04` |
| 25F1EBA | 83fa02 | wait 2 frames after FUN_140979650 (state +0xB994==2 of `+0x92D7930`) | GPU-op state machine | no | `83 fa 04` |
| 26278BA / 2627952 | 83f902 / 83f802 | wait 2 frames (stamp `+0x76B8E98`) | state machine 7/3 (FUN_145a7ecf0/ec90) | no | `83 f9 04` / `83 f8 04` |
| 32BE17D | 4883f802 | wait 2 frames (stamp `+0x7659A10`) | FUN_1432be0f0 capture state | no | `48 83 f8 04` |
| 32BE735 | 4183f802 | wait 2 frames (stamp `+0x7659A20`), then FUN_1432bc2f0 reads back | FUN_1432be6a0 | no | `41 83 f8 04` (do not touch the `41 b8 02..` state value after it) |
| 32BE0B1 | 83f803 | wait 3 frames (stamp `+0x76599C8`) | FUN_1432be090 | yes | `83 f8 04` |
| 12376E | 4883f903 | `frame - [+0x6B53610] > 2` | FUN_140123730: deferred work list `+0x8D42B50` | yes | `48 83 f9 04` |
| 11584B6 | 83f803 | entry age >= 3 -> remove entry, release obj at +0x10 | FUN_141158320 deferred release | yes | `83 f8 04` |
| 197836C / 1978379 / 1978384 | 83f903 / 83f903 / 83f803 | all 3 stamps (+0x1A8/1AC/1B0) older than 2 -> object may be destroyed | FUN_141978140 | yes | `83 f9 04` x2, `83 f8 04` |
| 16137AC | 83c0fd | init those stamps to frame-3 | FUN_1416136f0 | - | `83 c0 fc` (keep it = -(window)) |
| 4FB020F | 83c6fd | free immediately if last-use qword <= frame-3, else queue under lock `+0x760E180` | FUN_144fb0200 (deferred free of memory blocks; stamps written e.g. by 4DA8C45/4DA8CEB/4EE09D7/4EE09FC) | yes (just) | `83 c6 fc` |
| 3A171BA | 83f803 | age of [r14+0x18] >= 3 (frame copy at [rsp+0x34] from 3A17057) | FUN_143a16f20 | yes | `83 f8 04` |
| 55799A4 | 83c7fe | process entries whose stamp == frame-2 | FUN_1455798b0 (readback of entries stamped 2 frames ago) | no | `83 c7 fc` (equality: check that entries survive 2 more frames) |
| 9D9000 / 9D8F7D | 3b410c | state 1 -> 2 once stamp < frame (1 frame) | FUN_1409d8f20 job ring | n/a (1 frame) | leave |
| 1195F47 | 83ff1e | age >= 30 | FUN_141195990 | yes | - |
| 3A13CC3.. | 3de8030000 / 3d2c010000 | age > 1000 / 300 | FUN_143a13c80 streaming timeouts | yes | - |
| E81EB1 | | stamp < frame - [+0x76B23B4] | FUN_140e81c80 LRU, global threshold | yes (if threshold >= 3) | - |
| 12C55DA | | stamp < frame - [+0x7665D6C] | FUN_1412c55c0 LRU, global threshold | same | - |
| 2484470.. | 8d0c49 29c8 | slot = frame % 3, 0x48 B each, buffer `[+0x75DF268]` (0xD8 B from FUN_145cabb50), index `+0x75DF25C`, also read by 4ED46D0 | GPU timing queries (FUN_142483de0 takes the max of 3) | yes, not at 4 | for 4: `and eax,3`, a 0x120-B buffer, and a 4th read in 2483DE0 (diagnostic only) |

## 3. Other counter reads (no slot index)
* **Renamed dynamic buffers (3 versions)**, 46 sites, pattern `mov eax,[frame]; shl eax,4; xor eax,ver; test eax,0x7FFFFFF0; ...; FUN_144f244b0(count=[obj+0x54], v); lock cmpxchg [obj+0x50]`. Sites: 13548A, 93D9B7, 93D9EB, A9AB95, A9CDA5, F68CE1, F68D17, FC01B9, FC01EF, FC0225, FC025B, 11AC313, 1F8ED67, 2374282, 23742BA, 24E7B67, 24E7B9D, 25D8A5E, 261EB6D, 2760E44, 28DFC63, 2A6962F, 2A69666, 3277FEE, 3278022, 328F49D, 328F4D3, 32B05BF, 3367EA2, 3367ECD, 3367F01, 336A6AD, 336A6E6, 336A71C, 336A752, 33A1DD2, 33A1E08, 35940DC, 3594114, 4ADEF20, 4E73A80, 4F24419, 4F24453, 52004F7, 5378FD3, 5450E6A, 5450EA4. The init is 53F195C (`[obj+0x50] = frame<<4 | v | 0x80000000`). `FUN_144f244d0` returns **3** (or 1 when [obj+0x54]&8), so the version written in frame F equals the one from F-3. That is OK at 3 in flight and **not** at 4. Going to 4 would require a 4th copy per object (pointer table at obj+0x00.., per-version dwords at obj+0x40..) plus `mov eax,3` -> 4 at 4F244D0. This is **not** a simple byte patch.
* Command-block pool frame-byte stamps (equality only): 24749F8 (FUN_1424749d0, submit, writes `+0x75DF1E8[q]` and block+0x50), 55AC58D (FUN_1455ac580), 1F75EE1 (FUN_141f75eb0 returns unsubmitted blocks with fence -1, block+0x50 = frame). Safe at any depth.
* Equality / once-per-frame checks: B1B963 (xchg [+0x1D8]), 15D8A21 (`[rbx+0x154] != frame`), 28EE22A, 4FD2321 (above).
* Last-use stamps (consumers are the age checks above or LRUs): 2EF316, 34B244, 633C52, 1491392, A1C309, B39CB8, 119C84C, 1ACCF50, 11FCE2D, 1502DAE, 1529C3A, 152BD8E, 152C15D, 1CE58DF, 1195DC6, 2A7DBF0, 25F1F3B, 262782B, 32BE155, 32BE7BA, 12392F, 4DA8C45, 4DA8CEB, 4EE09D7, 4EE09FC, 53501A7, 5350458, 358F362 (stack), 3A17057 (stack), 13A686D (stack argument), 16EF5A0 (copied to `+0x840D904`).
* Frame value passed to command-list methods (markers or breadcrumbs): 35D60 (-> FUN_142484ad0, vtbl+0x250), 34F940 (-> FUN_14248ff40), 6C6656 / 7D2176 (vtbl+0x240).
* Other: 24747C0 (the increment), 247AF9C (reset to ebx), 1474EE0 (getter), 53914F2 (float), 53E6395 (hash seed), 2484193 (P, above).
* Second counter `+0xA9437B0` = back-buffer index mod `[+0xA9437B4]` (advanced in the limiter). It indexes the back-buffer array `[+0xA9437B8]` (11F9FF, CE05D9, 17C0B99, 2484716, 2484762, 2618412, 51D6FE0). It is not a frame-parity structure.

## 4. What the bitmaps track
FUN_140550fe0 runs once per frame and calls the 4 clear functions. Each one:
1. Takes the RW lock at base+0x250 (B1: `+0x8D44DB8`).
2. Walks a swiss-table hash map (groups of 16 ctrl bytes + 16 x 0x18-byte entries, 400 B per group; table pointer at base+0x248, capacity at base+0x240, count at base+0x244). Each entry is {key ptr -> u32 id, refcounted object (vtbl; +8 strong, +0xC weak)}.
3. For every entry whose object has **strong count == 1** (only the cache holds it), computes `bit = f(id)` and calls the "used recently" wrapper. If no slot has the bit, it **erases the entry and releases the object**: strong--, `vtbl+8` (destroy), weak--, `vtbl+0x10` (free).
4. After the eviction pass, clears bitmap[frame&1] with xchg (32 qwords = 2048 bits).

The setters run when an entry is bound for drawing (FUN_144e2f880 / FUN_144e89f90 from FUN_143261fd0, FUN_143266d10, FUN_14536d5f0, FUN_14515f6e0; B4 from the vtable method FUN_1435e4790). The `id` is an offset/index into the GPU table buffer created at init and bound through the header bind slot. In effect these are **bindless parameter or material-constant caches: each cached object owns a slot in a GPU-visible buffer, and the slot is freed and reused (CPU rewrites its contents) when the object is evicted.**

**Timing, which matters more than the slot count.** The counter increments at the end of the frame inside the limiter, and the clear runs after the eviction check:
* Marks made in frame U go to slot U&1.
* At U+1 and U+2 the check still sees them.
* At U+2 the slot is cleared, after the check.
* At **U+3** the object can be evicted.

Define *depth* = the number of earlier frames whose GPU work may still be pending while the CPU is in frame F. Stock depth is 1 (F-1). Your 3-in-flight patch gives depth 2 (F-1, F-2). A release in frame F of something last used in frame U is safe iff **F - U > depth**. For B1..B4, F - U = 3, so they are **still safe at depth 2**. The fact that frame N+1 shares a mark slot with N-1 is harmless because the check ORs both slots and the clear lags by one frame. They become unsafe only at depth >= 3, and then the 4-slot widening in section 1 applies. The same 3-frame window holds for **A**: the pair is retired in the frame it is replaced (last used in the previous frame), released 2 frames later in FUN_1406991e0 before rendering, so F - U = 3. A is a render target sized to the screen (+0x38/+0x3A compared with the current size). It is retired on every resolution change, including dynamic resolution, and recreated by FUN_1433a6910. The 3-version renamed buffers (section 3) also have F - U = 3.

None of B1..B4 is tied to the command-block pool (`+0x75DF1D8`) or to the page pools (`+0x75DF220/230/240`).

## 5. Ranked guess for the command-block overwrite (`+0x4E9B4B2` / `+0x4E9B54A` in FUN_144e9b480 / FUN_144e9b530, block from FUN_14005c280)
The recycle code reads the block's release list at block+0x48. Its nodes live **inside the block's own memory** (`[block+0x10] + [+0x75DF1CC] - 0x18`), and each node points at a linear-allocator page whose +0xC/+0x28 fields are read at 4E9B54A. A garbage node means either (a) the block memory was written by another user, or (b) a page or object it points to was freed. Block reuse is fence-gated (FUN_144dad000), so the suspects are frame-count based frees with a window of exactly 2 (F - U = 2). These are the only ones that break when depth goes from 1 to 2:

1. **Texture-streaming / defrag release queue** (`122147`, `43C5409`: `release = frame + 2`, consumed by FUN_144fd22d0). Old pages and resources (FUN_144fb0380, FUN_1443c4bf0, 4 KB page accounting) are released 2 frames after the move or residency change. That is the largest frame-delayed free path, and it peaks exactly during city load. Patch: `83 c0 02` -> `83 c0 03` (depth 2) or `83 c0 04` at both sites. Try this first.
2. **FUN_1425daae0 dead-entry compactor** (`25DAB58 41 83 fa 02`): destroys 0x110-byte entries and releases the refcounted objects they hold 2 frames after they were flagged dead. Patch `41 83 fa 03/04`.
3. **Unknown frame+2 deadline at F33642** (`83 c5 02`, FUN_140f32df0; consumer not traced).
4. 2-frame state machines and readbacks (25F1EBA, 26278BA/2627952, 32BE17D, 32BE735, 2A65AF4, 55799A4). These would give stale or half-written GPU data, but some of them then call FUN_140979650 / FUN_1432bc2f0 on the result, so a crash is possible. Patch each `02` -> `03/04` (and `fe` -> `fd/fc` at 55799A4).
5. B1..B4, A, and the 3-version renamed buffers are **safe at depth 2** (window 3); see section 4. Widen them only if you go deeper.
6. Limiter ring L: with only the compares raised it stalls rather than corrupts. Profiler table P gives garbage profiler numbers at depth 2, with no memory hazard.

Sites with a window of 3 (12376E, 11584B6, 1978xxx/16137AC, 4FB020F, 3A171BA, 32BE0B1) are fine at depth 2. Raise them only for depth 3 or more.
