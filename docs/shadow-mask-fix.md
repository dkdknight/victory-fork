# Shadow-mask traces: the host half of `VICTORY_SHADOW_ADDR`

The title's shadow-mask traces pick a ray-tracing structure's descriptor from
the view heap by a **computed** index and read only its title address, to walk
the structure through `SLOT_MEMORY`. The translator refused them at
`"a descriptor read only as data"` because `view_address` handles only a
**fixed** table descriptor (`a.dynamic.is_none()`). Those 5 programs not
translating is one of the two causes of black shadows.

The shader half is done (2000b30): `dynamic_view_address`
(`crates/victory-shader/src/emit/lower.rs`) computes the heap index and reads
the title address from a **third region of the strides table**,
`SLOT_STRIDES_ADDRESS_BYTES = 2*SLOT_STRIDES_COUNT*4`
(`crates/victory-shader/src/emit/shader.rs`), two dwords a heap slot (lo, hi).
Emitted only under `VICTORY_SHADOW_ADDR`. It uses `root_srv_handle(SLOT_STRIDES)`,
so a shader that hits the path declares `SLOT_STRIDES` and the host's existing
whole-heap-ready rule already makes the structures' memory ready.

## Shader half: what is proven

- **The programs.** `shaders.rpf` holds the five:
  `shadowmask:CS_TraceShadowRays_{Eighth,Quarter,Half,Full,FullAdaptive}`
  (`#96ccb14ef981fd85`, `#0e64f14fae060d04`, `#002e93a6cc2aa355`,
  `#07c93bfb77fecebb`, `#dbdbf36298727135`). Their shape: one
  `s_load_dwordx4` from the heap base `s[6:7]` with an SGPR `soffset`,
  `s_and_b32 hi, hi, 0xffff`, `s_lshr_b64 .., 8`; the result is title memory
  (`s_load_dword` of the header) and dword 0 of the node test's descriptor.
- **The computed-flags fallback is not reached.** Every node test of the five (22 sites) builds its
  descriptor with dword 1 `s_brev_b32 1` (0x80000000) and dword 3
  `s_mov_b32 0x890003ff`; only dword 0 (32-bit form) or the node pointer
  (64-bit form, `Full`, the all-memory descriptor) carries the address. So
  `resource.rs` classifies them `BvhNodes::Memory` and `bvh_address` takes
  dword 0 from the register at run time; the `BvhNodes::Resource` fallback
  for computed flag dwords is not hit. No change there (a `shortcut:` note).
- **On the real programs** (`victory translate shaders.rpf --list
  --validate`, fresh cache): off, compute 2677 of 2682 emitted, the five
  refused at "a descriptor read only as data"; with `VICTORY_SHADOW_ADDR=1`,
  2681 of 2682 emitted and validated by `dxil.dll`. `Eighth`, `Quarter`,
  `Half` and `Full` translate. `FullAdaptive` gets past that refusal and
  stops later, at 0xae80: "a lane mask holding lanes elsewhere read as data" (a
  different gap, open).
- **Nothing else moves.** `ref/corpus-cdxil`, `--validate`, off and on:
  2960 of 2960 translated both times; vertex/pixel 1287/1287, compute
  1108/1108, mesh 564/565 validated (72da0ba1efaec1e0, the 32 KiB output
  case), the same as before; with `--list` every program's hash (its DXIL
  and bindings) is the same off and on. The corpus has none of the five.
- **On the card** (`crates/victory-shader/src/emit/intersect_tests.rs`,
  `Descriptor::Heap`): the shadow-mask shape by hand (index = root constant 1
  times 32 in an SGPR `soffset`, address only, both forms), the strides table
  bound as root SRV `SLOT_STRIDES` at the host's full size with the
  structure's address at slot 5 only. With the switch on (the test override
  `TEST_SHADOW_ADDR`, no env race) both forms lower, validate, and match
  `crate::bvh` on the hardware adapter and WARP over the other node tests'
  323 cases; without the tests' stand-in they read through `SLOT_MEMORY`.
  Switch off: refused with "a descriptor read only as data". Checked to fail
  when the address sits at another slot or the high dword is read wrong.

Not proven: the host half below (nothing fills the table yet), and that the
traces then shade correctly in the game.

This note specifies the remaining host half, in `victory-gpu`. All of it is
gated on the same switch (read once, like `view_remap_on()` in
`crates/victory-gpu/src/descriptor/host.rs`). **It must be validated on the GPU
with the game** — it writes in the per-frame descriptor-mirror sweep (hot path),
which no automated test here exercises.

## 1. Grow the strides table (`descriptor/host.rs`, ~line 2124-2134)

Today:
```rust
let tables = if places.is_some() { 2 } else { 1 };
let bytes = u64::from(SLOT_STRIDES_COUNT) * 4 * tables;
```
The two regions are strides `[0, n)` and places `[n, 2n)` (`n = SLOT_STRIDES_COUNT`).
The address region is `[2n, 4n)` — two dwords a slot. Under the switch (and only
when `places.is_some()`, since the address path needs the heap):
```rust
let tables = if places.is_some() {
    if shadow_addr() { 4 } else { 2 }
} else { 1 };
```
`shadow_addr()`: a new `OnceLock<bool>` reading `VICTORY_SHADOW_ADDR == "1"`.
The buffer is bound exactly as now (same `SLOT_STRIDES` root SRV); only its size
grows. The mapping (`strides.2`) must cover `4n` dwords.

## 2. A per-slot writer (next to `write_place`, ~line 3878-3889)

```rust
/// Writes the title address (low, high) of the buffer view at heap slot
/// `index` into the strides table's third region, where the shadow-mask
/// path reads it (`SLOT_STRIDES_ADDRESS_BYTES`). Only with the table grown.
fn write_address(&self, index: u32, lo: u32, hi: u32) {
    let n = SLOT_STRIDES_COUNT;
    if let Some((_, _, data)) = &self.strides
        && self.address_region            // true when tables == 4 (store it on Host)
        && index < n
    {
        // SAFETY: inside the 4n-dword mapping; two aligned dwords at 2n + 2*index.
        unsafe {
            let p = (*data as *mut u32).add((2 * n + 2 * index) as usize);
            p.write_volatile(lo);
            p.add(1).write_volatile(hi);
        }
    }
}
```
Record `address_region: bool` on the heap's host state when the table is grown,
so the hot path checks a field, not the env each time.

## 3. Fill the address region in the mirror sweep

The title address of a view is already derivable:
`crate::descriptor::host::view_address(&view)` (used in
`crates/victory-gpu/src/draw.rs:1608`). Wherever the sweep writes a slot's
stride/place for a **buffer** view (the same per-view point that calls the
stride packer, `descriptor/host.rs:1568` region, and `write_place`), also:
```rust
if self.address_region {
    let (lo, hi) = view_address(&view).map_or((0, 0), |a| (a as u32, (a >> 32) as u32));
    self.write_address(index, lo, hi);
}
```
Non-buffer or absent slots write `(0, 0)`. (The shader does not test for 0:
a node test then walks from title address 0.) A cleared/given-back slot
(`place_back`) must also clear its address to `(0, 0)` — add a
`write_address(index, 0, 0)` there, and to `place_all`/the full refresh so a
grown table starts zeroed.

## 4. Memory readiness

Because the path uses `root_srv_handle(SLOT_STRIDES)`, the shader already
declares `SLOT_STRIDES`; `memory_ready` (`draw.rs:1619`) and the whole-heap-ready
rule already make ready the memory of the heap's buffer views, which includes
whatever structure the computed index selects. No change expected; confirm with
`VICTORY_BVH_TRACE` that the selected structures' pages are present (faults near
zero) once the switch is on.

## 5. Translator cache

The dynamic path changes emitted DXIL, so a shader translated with the switch
off is not valid with it on. The content-addressed cache key must include
`VICTORY_SHADOW_ADDR` (as other emission switches are keyed), or a fresh cache
dir must be used when enabling it. Verify `made=0` is NOT expected on the first
run after flipping the switch.
Not done yet: the switch list in `crates/victory-gpu/src/shader_cache.rs`
does not name `VICTORY_SHADOW_ADDR`.

## 6. GPU test of the shader path: done

See "Shader half: what is proven" above. The tests are
`a_structure_picked_from_the_heap_by_a_computed_index_matches_the_reference_on_the_card`
and `without_the_switch_a_structure_picked_from_the_heap_is_refused`. The
heap program declares a `RootType::ResourceHeap` parameter at s6; the strides
table is the shader's only root view (root parameter 1). The node test does
not take the loaded descriptor as its own (as this note's first plan had it):
used as a descriptor it never reaches the refused site, and the title's
programs do not do that.

## Validation checklist (on the GPU, with the game)

1. `VICTORY_SHADOW_ADDR=1` with `VICTORY_BVH_BUILDER=1` and a fresh cache.
2. `victory translate <shadow-mask programs> --validate` now emits them (was
   `"a descriptor read only as data"`; confirm with `VICTORY_DESC_WHY=1` off).
3. Interior/shadowed surfaces take ray-traced shadow, no device-loss.
4. `VICTORY_BVH_TRACE=1`: structure pages present, faults near zero.
5. A/B the frame cost (`VICTORY_AB`): the extra table fill is in the sweep.
