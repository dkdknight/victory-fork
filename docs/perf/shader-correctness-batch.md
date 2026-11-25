# Shader correctness batch (branch `shader/correctness-batch`)

Four findings of ubiquitarianism/leonida checked against our code, each
against AMD's "RDNA 2" Instruction Set Architecture manual (the reference
guide, 2020; section numbers below), LLVM's AMDGPU backend and the D3D12
documentation, before anything was changed. This batch is about correct
pictures, not frame rate: no fps change is expected.

| # | leonida | What | Verdict | Switch | Default |
|---|---------|------|---------|--------|---------|
| 1 | #1493, #978 | 32-bit literal as a signed 64-bit source | we were wrong (ISA 6.2.1) | `VICTORY_SHADER_SEXT64_LITERAL` | **on** (`=0` gives the old translation) |
| 2 | #966 | `ds_write2*` with equal offsets | we were wrong (ISA 10.4) | `VICTORY_SHADER_LDS_WRITE2_ONE` | **on** (`=0` gives the old translation) |
| 3 | #2089 | sampler MaxLOD below MinLOD | invalid D3D12 input; the right result is not documented | `VICTORY_SAMPLER_LOD_ORDER=1` (live key `sampler_lod_order`) | off |
| 4 | #1504 | null-resource barriers, clears of views without a resource | we never pass them to the host list | none (test added) | n/a |

## 1. Signed 64-bit literals (translator, default on)

ISA section 6.2.1, "Literal Expansion to 64 bits": literal constants are 32
bits; used as a 64-bit source, a 64-bit float gets them as its high dword,
"64-bit unsigned integer: zero extended to 64 bits", "64-bit signed integer:
sign extended to 64 bits". LLVM agrees:

- `llvm/docs/AMDGPUOperandSyntax.md`, "Conversion of Integer Values": i64
  gives `{-1,num.i32}`, u64/b64 `{0,num.u32}`, "performed by either assembler
  or AMDGPU H/W"; its example `s_bfe_i64 s[0:1], 0xffefffff, s3` reads
  `0xffffffffffefffff`.
- `SIInstrInfo::isOperandLegal`: a negative 32-bit literal on a 64-bit
  integer operand "will be encoded as its low 32 bits and then either
  correctly sign extended or incorrectly zero extended by HW". (The
  disassembler's `decodeLiteralConstant` still has a ToDo about signed 64-bit
  literals, so it says nothing either way.)

We zero-extended every literal (`src64` in `emit/lower.rs`, and the high
dword of a scalar 64-bit source in `sop`). Now the sources LLVM types as i64
sign-extend (`emit/rules.rs`, `signed_src64`):

- `v_cmp_*_i64` and `v_cmpx_*_i64`, both sources;
- `v_ashrrev_i64`, source 1 (the value; source 0 is the 32-bit count);
- `s_ashr_i64`, source 0;
- `v_mad_i64_i32` source 2, `s_bfe_i64` and `s_flbit_i32_i64` source 0: the
  rule names them, but we do not lower these three today (they are refused as
  before).

Unsigned sources (`v_cmp_*_u64`, `v_lshlrev_b64`, `v_lshrrev_b64`,
`v_mad_u64_u32`, `s_lshl_b64`, `s_lshr_b64`, `s_bfe_u64`, `s_cmp_*_u64`...)
still zero-extend. Inline constants (-16..64) were already full width.

## 2. `ds_write2` with equal offsets (translator, default on)

ISA section 10.4 (LDS access, double address instructions): "Specify only one
address by setting both offsets to the same value. This causes only one read
or write to occur and uses only the first DATA0." We stored DATA0 and then
DATA1 at the one address, so DATA1 won. Now `ds_write2_b32/b64` and
`ds_write2st64_b32/b64` with `offset0 == offset1` store DATA0 only. Reads
(`ds_read2*`) are unchanged: both halves read the one address, which is the
same value either way.

## 3. Sampler LOD order (GPU layer, off by default)

`D3D12_SAMPLER_DESC.MaxLOD`: "This value must be greater than or equal to
MinLOD". We passed the console's values through as they are, in three places:
console sampler descriptions (`sampler.rs`, `console`), the title's own
host-layout samplers (`descriptor/host.rs`, `View::Sampler`), and the
immediate samplers of translated shaders (root signature static samplers,
`pipeline/host.rs`). With `VICTORY_SAMPLER_LOD_ORDER=1` (or
`sampler_lod_order=1` in the live file; it applies to samplers made after the
change, not to ones already made or to root signatures already built) MaxLOD
is raised to MinLOD when below it, and a NaN MinLOD becomes 0 (a NaN MaxLOD
then MinLOD).

Off by default because neither D3D12 nor AMD documents what the hardware does
with an inverted pair: if a driver clamps as `min(max(lod, MinLOD), MaxLOD)`
the old pair samples at MaxLOD and the new one at MinLOD, and we do not know
which the console does. The counter runs with the switch off too, so one run
tells whether the title ever makes such a sampler. The switch is applied
outside the translation (the cached `StaticSampler` keeps the console's
values), so it does not need to be in the shader cache key.

## 4. Null-resource barriers and clears (checked, no change)

leonida #1504 is about two console calls that put a PC list in its error
state there, so that `Close` fails and the submission is dropped:

- **A barrier on no resource.** Our enhanced barriers (`barrier_groups`)
  never forward the title's groups: every call becomes one unordered access
  barrier on no resource (`UAV_BARRIER`), which D3D12 allows. Our legacy
  barriers (`barrier`, `host_barrier`): a transition on no resource has no
  host texture (`HostList::transition` returns `None`) and is left out; an
  aliasing or UAV barrier with a null resource is legal D3D12 and passes as
  it is.
- **A clear of a view made without a resource.** `create_rtv`/`create_dsv`
  record the view with a null resource in the title's descriptor memory (so
  the title's own copies of the record follow it, the gap leonida lists),
  and `clear_rtv`/`clear_dsv` stop at `host_resource(null)`, before any
  host call.

So we cannot put a list in error this way. The unit test
`list::tests::null_resource_views_and_barriers_reach_no_host_call` pins that
down.

## Switches and the shader cache

`VICTORY_SHADER_SEXT64_LITERAL` and `VICTORY_SHADER_LDS_WRITE2_ONE` are read in
`crates/victory-shader/src/emit/rules.rs`. `build.rs` collects every
`"VICTORY_*"` name in the translator's source into `VICTORY_SHADER_SWITCHES`,
and `translation_fingerprint` adds each one's value, so the shader cache keys
on them: changing either gives new translations. The rules also change the
translator's source hash, so **this branch's first run translates every
shader again** (bake test first).

## Log lines

At the first shader cache key (start):

```
INFO translator correctness rules (keyed in the shader cache): VICTORY_SHADER_SEXT64_LITERAL=1 VICTORY_SHADER_LDS_WRITE2_ONE=1
```

and a `WARN a translator correctness rule is off: ...` when one is `=0`.

Each second (with the per-frame lines), only when translations in that second
met a rule:

```
INFO translator rules in this second's translations: 3 negative literals read as signed 64-bit sources, 3 sign-extended (VICTORY_SHADER_SEXT64_LITERAL); 1 paired LDS writes at one address, 1 write DATA0 only (VICTORY_SHADER_LDS_WRITE2_ONE); since start 12/12 and 4/4
```

Shaders taken from the cache are not translated, so they are not counted: the
warm-up (bake) is where these numbers show. `victory translate --warm` prints
the totals at its end:

```
warm-up      translator rules (VICTORY_SHADER_SEXT64_LITERAL=1 VICTORY_SHADER_LDS_WRITE2_ONE=1): 37 negative literals read as signed 64-bit sources, 37 sign-extended; 5 paired LDS writes at one address, 5 write DATA0 only
```

The counts are per instruction lowered: a program lowered twice (a retry with
first-half pairs, the two halves of a console wave of 64) counts twice.

Each second, only when samplers with inverted LODs were made:

```
INFO samplers made with MaxLOD below MinLOD (or a NaN LOD): 2 this second (0.2 per frame), 0 of them given MaxLOD = MinLOD (VICTORY_SAMPLER_LOD_ORDER)
```

and once, when the switch is on: `INFO VICTORY_SAMPLER_LOD_ORDER=1: a sampler's
MaxLOD is raised to its MinLOD when below it`.

## Tests

- `emit::valu_tests::v_cmp_i64_sign_extends_a_literal`,
  `v_ashrrev_i64_sign_extends_a_literal`, `s_ashr_i64_sign_extends_a_literal`
  and `ds_write2_with_equal_offsets_keeps_data0`: synthetic shaders, run on
  the card and on WARP, each translated with its rule on and off (the off
  run checks the old result, so the test shows the rule is what changes it).
  The u64/b64 forms are controls (unchanged by the rule).
- `emit::rules::tests`: which sources are signed, the switch parsing, the
  counters' decisions.
- `sampler::tests::inverted_lods_are_put_in_order`,
  `list::tests::null_resource_views_and_barriers_reach_no_host_call`.

`cargo test --release -p victory-shader --lib`: 252 passed (2 ignored).
`cargo test --release -p victory-gpu --lib`: all pass. `cargo build --release
-p victory`, also with `--features diagnostics`: clean (one older warning,
`memo_say`).

## Risks

- Rules 1 and 2 are on by default and change translations. They follow the
  ISA manual, but a shader that "worked" by accident with the old result
  would now differ. `VICTORY_SHADER_SEXT64_LITERAL=0` and
  `VICTORY_SHADER_LDS_WRITE2_ONE=0` give the old translation back (new cache
  keys, so a run with them translates again).
- How often the title meets them is unknown until the bake run: the warm-up
  line says. Zero means no change in this title.
- The sampler switch is off; turned on, it may change which mip an inverted
  sampler reads (the result is not documented either way).
- First run of this branch: every shader translates again (source hash).

## Credits

Findings from ubiquitarianism/leonida: #1493 (QemmHD, vector literals), #978
(OlaiFG7, scalar literals, finding by akimfomin), #966 (akimfomin, LDS paired
writes; links THE-PRO-GAME's finding in Marvisak/VIctory #1088), #2089
(KushGene, sampler LOD order), #1504 (null barriers and clears). Our code is
written anew; each rule was checked against the AMD and LLVM sources above.
