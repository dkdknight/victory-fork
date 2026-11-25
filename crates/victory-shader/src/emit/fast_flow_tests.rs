//! `VICTORY_SHADER_FAST_FLOW` `faults` and `parked` ([`super::fast_flow`]) end to end: a
//! console wave32 compute program reading title memory by address (`global_load_dword`, one
//! of them under a guard, and `s_load_dword`), lowered with and without the switch, validated
//! by `dxil.dll` when present, and run on the host GPU and WARP with a directory of pages, a
//! copy of one page and the faults buffer bound as the host binds them (`shader.rs`
//! `SLOT_MEMORY`). The values read and the pages recorded are the same either way; with the
//! switch, an access whose page has a copy writes nothing to the faults buffer (its sink,
//! dword 0, stays zero).

use super::host::{Dispatch, Host, Root, View};
use super::shader::{MEMORY_FAULTS, MEMORY_PAGES, SLOT_FAULTS, SLOT_MEMORY, USER_MEMORY_BASE};
use super::{HostShader, fast_flow, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;
const VCC: u32 = 106;
const EXEC: u32 = 126;
const LIT: u32 = 255;

const fn v(n: u32) -> u32 {
    256 + n
}

const fn k(n: u32) -> u32 {
    128 + n
}

const fn sop1(op: u32, sdst: u32, src0: u32) -> u32 {
    0xBE80_0000 | sdst << 16 | op << 8 | src0
}

const fn vop1(op: u32, vdst: u32, src0: u32) -> u32 {
    0x7E00_0000 | vdst << 17 | op << 9 | src0
}

const fn vop2(op: u32, vdst: u32, src0: u32, vsrc1: u32) -> u32 {
    op << 25 | vdst << 17 | vsrc1 << 9 | src0
}

const fn vopc(op: u32, src0: u32, vsrc1: u32) -> u32 {
    0x7C00_0000 | op << 17 | vsrc1 << 9 | src0
}

/// `global_load_dword v<dst>, v[addr:addr+1], off`
const fn global_load(dst: u32, addr: u32) -> [u32; 2] {
    [0x37 << 26 | 0x0C << 18 | 2 << 14, addr | 0x7D << 16 | dst << 24]
}

/// `buffer_store_dword v<data>, v<addr>, s[8:11], 0 offen offset:<offset>`
const fn store(data: u32, addr: u32, offset: u32) -> [u32; 2] {
    [0xE070_1000 | offset, 0x8002_0000 | data << 8 | addr]
}

/// Title address of the page with a copy (page 3); page 5 has none.
const BASE: u32 = 3 << 16;
const MISSING_PAGE: u32 = 5;

/// 32 threads, thread t (v0): v4 = the dword at BASE + 4t for t < 16 under a guard (EXEC
/// narrowed without a branch), else 0xDEAD; v5 = the dword at BASE + 4t, or at page 5 for odd
/// t; v9 = the dword at BASE + 8 (`s_load_dword`); each thread stores v4, v5, v9, t.
fn program() -> NativeProgram {
    let mut w = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        125 << 25,               // s_load_dwordx4 s[8:11], s[4:5], 0
        vop2(0x1A, 1, k(2), 0),  // v_lshlrev_b32 v1, 2, v0
        vop2(0x25, 2, 0, 1),     // v_add_nc_u32 v2, s0, v1
        vop1(0x01, 3, 1),        // v_mov_b32 v3, s1
        vop1(0x01, 4, LIT),      // v_mov_b32 v4, 0xDEAD
        0xDEAD,
        vopc(0xC4, k(16), 0),    // v_cmp_gt_u32 vcc, 16, v0
        sop1(0x3C, 12, VCC),     // s_and_saveexec_b32 s12, vcc_lo (no skip: a guard)
    ];
    w.extend(global_load(4, 2));
    w.extend([
        sop1(0x03, EXEC, 12),    // s_mov_b32 exec_lo, s12
        vop2(0x1B, 8, k(1), 0),  // v_and_b32 v8, 1, v0
        vop2(0x1A, 8, k(17), 8), // v_lshlrev_b32 v8, 17, v8: two pages on for odd threads
        vop2(0x25, 6, v(2), 8),  // v_add_nc_u32 v6, v2, v8
        vop1(0x01, 7, 1),        // v_mov_b32 v7, s1
    ]);
    w.extend(global_load(5, 6));
    w.extend([
        0x3D << 26 | 16 << 6, // s_load_dword s16, s[0:1], 8
        8 | 125 << 25,
        vop1(0x01, 9, 16),       // v_mov_b32 v9, s16
        vop2(0x1A, 15, k(4), 0), // v_lshlrev_b32 v15, 4, v0
    ]);
    for (n, r) in [4u32, 5, 9, 0].into_iter().enumerate() {
        w.extend(store(r, 15, 4 * n as u32));
    }
    w.push(S_ENDPGM);
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
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1,
            threads: [32, 1, 1],
        }),
        code: w.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// [`program`] lowered with the switch's `parts`, validated when `dxil.dll` is there.
fn lowered(parts: u32) -> HostShader {
    let native = program();
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(&p, &native).expect("io::raise");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let l = fast_flow::with_parts(parts, || {
        lower::lower(&lower::Parts {
            native: &native,
            p: &p,
            st: &st,
            io: &io,
            res: &res,
            inputs: Vec::new(),
            outputs: Vec::new(),
            prims: Vec::new(),
            mesh_stride: 0,
        })
    });
    lower::TEST_HOST_WAVE.set(was);
    let l = l.expect("lower");
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: l.dxil,
        user_data: l.user_data,
        samplers: l.samplers,
        static_samplers: Vec::new(),
        root_cbvs: l.root_cbvs,
        root_srvs: l.root_srvs,
        root_uavs: l.root_uavs,
        root_indexed_views: Vec::new(),
    };
    assert!(shader.root_srvs.contains(&SLOT_MEMORY), "no directory: not memory by address");
    assert!(shader.root_uavs.contains(&SLOT_FAULTS), "no faults buffer");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone()).expect("DXIL validation failed");
    }
    shader
}

/// The dword at `BASE + 4 n` in the copy.
fn data(n: u32) -> u32 {
    0x1000 + n
}

/// Runs `shader` on each SM 6.6 device (hardware, WARP): (device, the 128 dwords stored, the
/// faults buffer).
fn run(shader: &HostShader) -> Vec<(&'static str, Vec<u32>, Vec<u32>)> {
    // the directory: page 3 is copy 1 from its byte 0
    let mut directory = vec![0u32; MEMORY_PAGES as usize * 2];
    directory[(BASE >> 16) as usize * 2] = 1;
    let copy: Vec<u32> = (0..16384).map(data).collect();
    let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
    for u in &shader.user_data {
        match (u.root, u.slot, u.dword) {
            (RootType::Constants, 0, 0) => ud[u.index as usize] = BASE,
            (RootType::Other(USER_MEMORY_BASE), ..) => ud[u.index as usize] = 1,
            _ => {}
        }
    }
    // root parameters: the user data, then the root views by slot (faults 253, directory 254)
    let views = [View::RawUav(128), View::RawUav(16384), View::RawUavInit(&copy)];
    let mut out = Vec::new();
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host
            .compute_pipeline(shader)
            .unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        let got = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![
                        (0, Root::Constants(ud.clone())),
                        (1, Root::Uav(MEMORY_FAULTS)),
                        (
                            2,
                            Root::View {
                                srv: true,
                                data: directory.clone(),
                            },
                        ),
                    ],
                    views: &views,
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        out.push((label, got[0].clone(), got[3].clone()));
    }
    out
}

#[test]
fn fast_flow_reads_the_same_and_writes_faults_only_for_pages_without_a_copy() {
    let before = lowered(0);
    let parked = fast_flow::counts()[fast_flow::PARKED_N];
    let branched = fast_flow::counts()[fast_flow::BRANCHED];
    let fast = lowered(fast_flow::FAULTS | fast_flow::PARKED);
    assert!(fast.dxil != before.dxil, "the switch changed nothing");
    // (other tests may lower too, so only "more than before")
    assert!(fast_flow::counts()[fast_flow::PARKED_N] > parked, "the guarded load was not parked");
    assert!(fast_flow::counts()[fast_flow::BRANCHED] >= branched + 3, "three accesses, three records deferred");
    let want: Vec<u32> = (0..32u32)
        .flat_map(|t| {
            let guarded = if t < 16 { data(t) } else { 0xDEAD };
            let other = if t % 2 == 0 { data(t) } else { 0 };
            [guarded, other, data(2), t]
        })
        .collect();
    let place = (MISSING_PAGE % (MEMORY_FAULTS - 1) + 1) as usize;
    let runs = [("before", run(&before)), ("fast flow", run(&fast))];
    for (name, results) in &runs {
        for (device, stored, faults) in results {
            assert_eq!(stored, &want, "{name} on {device}: the values read");
            assert_eq!(faults[place], MISSING_PAGE, "{name} on {device}: page 5 recorded");
            let others = faults.iter().enumerate().skip(1).filter(|&(i, &f)| i != place && f != 0).count();
            assert_eq!(others, 0, "{name} on {device}: no other page recorded");
            eprintln!("{name} on {device}: the console's values, page {MISSING_PAGE} recorded; sink {:#x}", faults[0]);
        }
    }
    // before, every access wrote the sink (a page number); with the switch nothing did
    for (device, _, faults) in &runs[1].1 {
        assert_eq!(faults[0], 0, "fast flow on {device}: an access with a copy wrote the faults buffer");
    }
    for (device, _, faults) in &runs[0].1 {
        assert_ne!(faults[0], 0, "before, on {device}: the sink was not written (the test shows nothing)");
    }
}
