# Shadow-mask traces: the host half of `VICTORY_SHADOW_ADDR`

The title's shadow-mask traces pick a ray-tracing structure's descriptor from
the view heap by a **computed** index and read only its title address, to walk
the structure through `SLOT_MEMORY`. The translator refused them at
`"a descriptor read only as data"` because `view_address` handles only a
**fixed** table descriptor (`a.dynamic.is_none()`). Those ~5 programs not
translating is one of the two causes of black shadows.

The shader half is done (commit on `coolfox/ultimate3`): `dynamic_view_address`
(`crates/victory-shader/src/emit/lower.rs`) computes the heap index and reads
the title address from a **third region of the strides table**,
`SLOT_STRIDES_ADDRESS_BYTES = 2*SLOT_STRIDES_COUNT*4`
(`crates/victory-shader/src/emit/shader.rs`), two dwords a heap slot (lo, hi).
Emitted only under `VICTORY_SHADOW_ADDR`. It uses `root_srv_handle(SLOT_STRIDES)`,
so a shader that hits the path declares `SLOT_STRIDES` and the host's existing
whole-heap-ready rule already makes the structures' memory ready.

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
Non-buffer or absent slots write `(0, 0)` (the shader treats 0 as "none", like
the static path's warning). A cleared/given-back slot (`place_back`) must also
clear its address to `(0, 0)` — add a `write_address(index, 0, 0)` there, and to
`place_all`/the full refresh so a grown table starts zeroed.

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

## 6. GPU test of the shader path (`crates/victory-shader/src/emit/intersect_tests.rs`)

Add a `Descriptor::Heap` case to the harness:
- A `RootParam { root: RootType::ResourceHeap, .. }` for the heap base.
- Program: compute an index in an SGPR, `s_load_dwordx4` the structure
  descriptor from the heap at `index*32` (a non-NULL `soffset`, so the tracker
  marks it `a.dynamic.is_some()`), then `image_bvh*` on it as the other cases do.
- Bind `SLOT_STRIDES` as a root SRV over a buffer whose third region
  (`SLOT_STRIDES_ADDRESS_BYTES`) holds, at `2*index`, the `TEST_MEMORY` base
  address of the node scene (lo, hi).
- Assert, with `VICTORY_SHADOW_ADDR=1`, the program lowers and the node test
  matches the reference on the card — the regression test the fix needs. With
  the switch off, assert it still returns `"a descriptor read only as data"`.

## Validation checklist (on the GPU, with the game)

1. `VICTORY_SHADOW_ADDR=1` with `VICTORY_BVH_BUILDER=1` and a fresh cache.
2. `victory translate <shadow-mask programs> --validate` now emits them (was
   `"a descriptor read only as data"`; confirm with `VICTORY_DESC_WHY=1` off).
3. Interior/shadowed surfaces take ray-traced shadow, no device-loss.
4. `VICTORY_BVH_TRACE=1`: structure pages present, faults near zero.
5. A/B the frame cost (`VICTORY_AB`): the extra table fill is in the sweep.
