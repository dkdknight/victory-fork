//! `VICTORY_SHADER_FAST_MEM` ([`super::fast_mem`]) end to end: synthetic
//! GFX10 compute programs lowered with and without it, validated by
//! `dxil.dll` when present, run on the host GPU and WARP, and compared with
//! what the console computes (each run skipped without a device).
//!
//! - title memory by address (`global_load_*`, `global_store_dword`)
//!   through the host's directory, with every page there and with one
//!   missing: the same bytes read and written, the same fault notes, and
//!   with the switch nothing at the notes' sink;
//! - a constant buffer read per lane at an index (`buffer_load_dwordx3`,
//!   `buffer_load_dwordx4` on a CBV), across rows;
//! - a buffer picked from the view heap read in two blocks (the element
//!   stride from the host's table by heap slot).

use super::host::{Dispatch, Host, Root, Stride, View};
use super::lower::TEST_FAST_MEM;
use super::shader::{MEMORY_FAULTS, USER_MEMORY_BASE};
use super::{FastMem, HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;
/// Threads of the one group each program runs.
const THREADS: u32 = 32;

/// A wave32 compute program of [`THREADS`] threads: 4 root constants in
/// s0..s3 and, in s4, the root parameter `table` (a descriptor table, or
/// the view heap) with these used-resource entries.
fn native(words: &[u32], table: RootType, used: Vec<UsedResource>) -> NativeProgram {
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 16,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used,
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1, // 5 user SGPRs: s0..s4
            threads: [THREADS, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// A table entry of `kind` at dword `at`.
fn entry(kind: ResourceType, at: u32) -> UsedResource {
    UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords: at,
        lower_bound: 0,
        stride: 4,
        dwords: 4,
        user_data: 4,
        slot: 1,
    }
}

/// `native` lowered with the switch `fast`, validated when the validator
/// is there; what the switch changed.
fn build(native: &NativeProgram, fast: bool) -> (HostShader, FastMem) {
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, native).expect("io::raise");
    TEST_FAST_MEM.set(Some(fast));
    let lowered = lower::lower(&lower::Parts {
        native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    });
    TEST_FAST_MEM.set(None);
    let lowered = lowered.expect("the program lowers");
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        samplers: lowered.samplers,
        static_samplers: Vec::new(),
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
        root_indexed_views: res.root_indexed_views.clone(),
    };
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .unwrap_or_else(|e| panic!("DXIL validation failed (fast {fast}): {e}"));
    } else {
        eprintln!("no dxil.dll; validation skipped");
    }
    (shader, lowered.fast)
}

/// The user data of `shader`: the root constants `s` in s0..s3, the table
/// at heap index 0, and `memory_base` for the host's word of the memory
/// copies' first view.
fn user_data(shader: &HostShader, s: [u32; 4], memory_base: u32) -> Vec<u32> {
    let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        ud[u.index as usize] = match (u.root, u.dword) {
            (RootType::Constants, d) => s[d as usize],
            (RootType::Other(USER_MEMORY_BASE), _) => memory_base,
            _ => 0,
        };
    }
    ud
}

/// Runs `shader` on the hardware GPU and WARP (each skipped without a
/// device): per one that ran, every UAV's dwords (the heap's in order,
/// then the root UAVs').
fn run(
    shader: &HostShader,
    roots: impl Fn() -> Vec<(u32, Root)>,
    views: &[View],
) -> Vec<(&'static str, Vec<Vec<u32>>)> {
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host
            .compute_pipeline(shader)
            .unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        let words = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: roots(),
                    views,
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        out.push((label, words));
    }
    out
}

/// Title memory by address: thread t reads around `A = s0 + 4t` (s1 the
/// high dword): a dword at A and A + 4, two at A + 64, one at A - 4 (an
/// access before the address), and writes its id at A + 512. The results
/// go to the table's UAV, 8 dwords a thread.
fn memory_program() -> Vec<u32> {
    vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0xF408_0302,
        0xFA00_0000, // s_load_dwordx4 s[12:15], s[4:5], 0x0
        0x3406_0082, // v_lshlrev_b32 v3, 2, v0
        0x4A02_0600, // v_add_nc_u32 v1, s0, v3
        0x7E04_0201, // v_mov_b32 v2, s1
        0xDC30_8000,
        0x047D_0001, // global_load_dword v4, v[1:2], off
        0xDC30_8004,
        0x057D_0001, // global_load_dword v5, v[1:2], off offset:4
        0xDC34_8040,
        0x067D_0001, // global_load_dwordx2 v[6:7], v[1:2], off offset:64
        0xDC30_9FFC,
        0x087D_0001, // global_load_dword v8, v[1:2], off offset:-4
        0xDC70_8200,
        0x007D_0001, // global_store_dword v[1:2], v0, off offset:512
        0x3412_0085, // v_lshlrev_b32 v9, 5, v0
        0xE078_1000,
        0x8003_0409, // buffer_store_dwordx4 v[4:7], v9, s[12:15], 0 offen
        0xE070_1010,
        0x8003_0809, // buffer_store_dword v8, v9, s[12:15], 0 offen offset:16
        S_ENDPGM,
    ]
}

/// What the title's memory holds at `address` in [`memory_runs`].
fn title_word(address: u32) -> u32 {
    address ^ 0x5A5A_0000
}

/// The copies of [`memory_runs`]: page 2 (0x20000..) at byte 0x100 of
/// copy 1, page 3 at byte 0x40 of copy 2.
const COPY1_AT: u32 = 0x100;
const COPY2_AT: u32 = 0x40;

/// Runs [`memory_program`] with its first address at 0x2FFC0 (half the
/// threads on page 2, half on page 3), page 3 there or not; per device:
/// the table's UAV, copy 2 and the faults.
fn memory_runs(fast: bool, page3: bool) -> Vec<(&'static str, [Vec<u32>; 3])> {
    let native = native(&memory_program(), RootType::Table, vec![entry(ResourceType::Uav, 0)]);
    let (shader, f) = build(&native, fast);
    assert!(shader.root_srvs.contains(&super::SLOT_MEMORY));
    assert!(shader.root_uavs.contains(&super::SLOT_FAULTS));
    if fast {
        // Every access notes behind a branch; the four loads and the
        // store share the look at A.
        assert_eq!(f.notes, 5, "{f:?}");
        assert_eq!(f.shared_looks, 4, "{f:?}");
    } else {
        assert_eq!(f, FastMem::default());
    }
    let page = |at: u32| -> Vec<u32> {
        let mut words = vec![0u32; ((at + 0x1_0000) / 4) as usize];
        let base = if at == COPY1_AT { 0x2_0000 } else { 0x3_0000 };
        for k in 0..0x4000u32 {
            words[(at / 4 + k) as usize] = title_word(base + 4 * k);
        }
        words
    };
    let (copy1, copy2) = (page(COPY1_AT), page(COPY2_AT));
    // Directory: two dwords a page (copy number, the page's byte in it).
    let mut directory = vec![0u32; 16];
    directory[4..6].copy_from_slice(&[1, COPY1_AT]);
    if page3 {
        directory[6..8].copy_from_slice(&[2, COPY2_AT]);
    }
    // Heap: the results, the null copy (zeros; reads past it give 0),
    // copies 1 and 2.
    let views = [
        View::RawUav(8 * THREADS),
        View::RawUav(4),
        View::RawUavData(&copy1),
        View::RawUavData(&copy2),
    ];
    let ud = user_data(&shader, [0x2_FFC0, 0, 0, 0], 1);
    // Root views after the user data, by register: u253 the faults, t254
    // the directory.
    let roots = || {
        vec![
            (0, Root::Constants(ud.clone())),
            (1, Root::Uav(MEMORY_FAULTS)),
            (
                2,
                Root::View {
                    srv: true,
                    data: directory.clone(),
                },
            ),
        ]
    };
    run(&shader, roots, &views)
        .into_iter()
        .map(|(label, w)| (label, [w[0].clone(), w[3].clone(), w[4].clone()]))
        .collect()
}

/// The value thread `t` should see at `address`, page 3 there or not.
fn seen(address: u32, page3: bool) -> u32 {
    if address >= 0x3_0000 && !page3 { 0 } else { title_word(address) }
}

fn check_memory(fast: bool, page3: bool) {
    for (label, [out, copy2, faults]) in memory_runs(fast, page3) {
        for t in 0..THREADS {
            let a = 0x2_FFC0 + 4 * t;
            let want = [a, a + 4, a + 64, a + 68, a - 4].map(|x| seen(x, page3));
            let got = &out[(8 * t) as usize..(8 * t + 5) as usize];
            assert_eq!(got, want, "{label}, fast {fast}, page 3 {page3}, thread {t}");
            // The store at A + 512 (page 3), dropped without the page.
            let at = ((COPY2_AT + a + 512 - 0x3_0000) / 4) as usize;
            let stored = if page3 { t } else { title_word(a + 512) };
            assert_eq!(copy2[at], stored, "{label}, fast {fast}, page 3 {page3}, store of thread {t}");
        }
        // Page 3 noted (at its place, page % 4095 + 1) only when missing.
        assert_eq!(faults[4], if page3 { 0 } else { 3 }, "{label}, fast {fast}");
        let others = faults
            .iter()
            .enumerate()
            .filter(|&(i, &x)| i != 0 && i != 4 && x != 0)
            .count();
        assert_eq!(others, 0, "{label}, fast {fast}: notes elsewhere");
        if fast {
            // Nothing at the sink: no lane stored a note it did not have.
            assert_eq!(faults[0], 0, "{label}: the sink was written");
        } else {
            // Without the switch every lane of every access stores there
            // (a page number, 2 or 3): what the switch saves.
            assert!(matches!(faults[0], 2 | 3), "{label}: sink {:#x}", faults[0]);
        }
    }
}

#[test]
fn title_memory_by_address_reads_and_writes_the_same_with_fast_memory_accesses() {
    for fast in [false, true] {
        check_memory(fast, true);
    }
}

#[test]
fn a_page_without_a_copy_reads_zeros_and_is_noted_with_fast_memory_accesses() {
    for fast in [false, true] {
        check_memory(fast, false);
    }
}

/// A constant buffer read per lane: thread t reads four dwords at byte
/// `16 (t & 7) + 8` and three at `16 (t & 7) + 4` of the table's CBV, and
/// writes them to the table's UAV, 8 dwords a thread.
fn cbv_program() -> Vec<u32> {
    vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0xF408_0202,
        0xFA00_0000, // s_load_dwordx4 s[8:11], s[4:5], 0x0
        0xF408_0302,
        0xFA00_0020, // s_load_dwordx4 s[12:15], s[4:5], 0x20
        0x3602_0087, // v_and_b32 v1, 7, v0
        0xE038_2008,
        0x8002_0201, // buffer_load_dwordx4 v[2:5], v1, s[8:11], 0 idxen offset:8
        0xE03C_2004,
        0x8002_0601, // buffer_load_dwordx3 v[6:8], v1, s[8:11], 0 idxen offset:4
        0x3412_0085, // v_lshlrev_b32 v9, 5, v0
        0xE078_1000,
        0x8003_0209, // buffer_store_dwordx4 v[2:5], v9, s[12:15], 0 offen
        0xE07C_1010,
        0x8003_0609, // buffer_store_dwordx3 v[6:8], v9, s[12:15], 0 offen offset:16
        S_ENDPGM,
    ]
}

#[test]
fn a_constant_buffer_read_per_lane_reads_each_row_once_with_fast_memory_accesses() {
    let cbv = UsedResource {
        stride: 0,
        ..entry(ResourceType::Cbv, 0)
    };
    let native = native(&cbv_program(), RootType::Table, vec![cbv, entry(ResourceType::Uav, 8)]);
    let data: Vec<u32> = (0..64).map(|k| 0x100 + k).collect();
    for fast in [false, true] {
        let (shader, f) = build(&native, fast);
        // dwordx4 at 8: two rows, two dwords each; dwordx3 at 4: one row.
        assert_eq!(f.cbv_dwords, if fast { 4 } else { 0 }, "{f:?}");
        let ud = user_data(&shader, [0; 4], 0);
        let views = [View::Cbv(&data), View::RawUav(8 * THREADS)];
        for (label, w) in run(&shader, || vec![(0, Root::Constants(ud.clone()))], &views) {
            for t in 0..THREADS {
                let row = 4 * (t & 7);
                let want = [row + 2, row + 3, row + 4, row + 5, row + 1, row + 2, row + 3]
                    .map(|k| data[k as usize]);
                let got = &w[0][(8 * t) as usize..(8 * t + 7) as usize];
                assert_eq!(got, want, "{label}, fast {fast}, thread {t}");
            }
        }
    }
}

/// A buffer picked from the view heap at slot s0 (its element stride from
/// the host's table by heap slot), read by element in two blocks (the
/// second runs when s1 is not 0): thread t's element is dwords 2t and
/// 2t + 1 with a stride of 8. The UAV at heap slot 1 takes both, 2 dwords
/// a thread.
fn heap_program() -> Vec<u32> {
    vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0x8F08_8500, // s_lshl_b32 s8, s0, 5
        0xF408_0202,
        0x1000_0000, // s_load_dwordx4 s[8:11], s[4:5], s8
        0xF408_0302,
        0xFA00_0020, // s_load_dwordx4 s[12:15], s[4:5], 0x20
        0x7E04_0280, // v_mov_b32 v2, 0
        0xE030_2000,
        0x8002_0100, // buffer_load_dword v1, v0, s[8:11], 0 idxen
        0xBF06_8001, // s_cmp_eq_u32 s1, 0
        0xBF85_0002, // s_cbranch_scc1 +2
        0xE030_2004,
        0x8002_0200, // buffer_load_dword v2, v0, s[8:11], 0 idxen offset:4
        0x3406_0083, // v_lshlrev_b32 v3, 3, v0
        0xE074_1000,
        0x8003_0103, // buffer_store_dwordx2 v[1:2], v3, s[12:15], 0 offen
        S_ENDPGM,
    ]
}

#[test]
fn a_heap_buffers_stride_is_read_once_per_descriptor_with_fast_memory_accesses() {
    let native = native(&heap_program(), RootType::ResourceHeap, Vec::new());
    let data: Vec<u32> = (0..2 * THREADS).map(|k| 0x7000 + k).collect();
    // The host's table by heap slot: slot 2 holds elements of 8 bytes.
    let strides = vec![0, 0, 8, 0];
    for fast in [false, true] {
        let (shader, f) = build(&native, fast);
        assert!(shader.root_srvs.contains(&super::SLOT_STRIDES));
        // The second block's read takes the first's lookup.
        assert_eq!(f.strides, u32::from(fast), "{f:?}");
        for second in [0u32, 1] {
            let ud = user_data(&shader, [2, second, 0, 0], 0);
            let roots = || {
                vec![
                    (0, Root::Constants(ud.clone())),
                    (
                        1,
                        Root::View {
                            srv: true,
                            data: strides.clone(),
                        },
                    ),
                ]
            };
            let views = [
                View::RawUav(4),
                View::RawUav(2 * THREADS),
                View::Buffer {
                    data: &data,
                    stride: Stride::Raw,
                },
            ];
            for (label, w) in run(&shader, roots, &views) {
                for t in 0..THREADS as usize {
                    let want = [data[2 * t], if second != 0 { data[2 * t + 1] } else { 0 }];
                    assert_eq!(w[1][2 * t..2 * t + 2], want, "{label}, fast {fast}, s1 {second}, thread {t}");
                }
            }
        }
    }
}
