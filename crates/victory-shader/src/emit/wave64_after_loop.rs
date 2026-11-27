// ISA encoders from fork PR #993 and loop fixture from darkworldhm 08fdf0b.
use super::*;

#[test]
fn lane_loop_exit_includes_repeated_postdominators() {
    use crate::structure::{Atom, LaneExit};
    let native = super::program(0xBF88_0000);
    let ir = crate::spill::build(&native).unwrap();
    let mut st = structure::structure(&ir).unwrap();
    let when = st
        .exits
        .iter()
        .find_map(|exit| match exit {
            LaneExit::Branch { when, .. }
                if st
                    .bools
                    .support(*when)
                    .iter()
                    .any(|a| !matches!(a, Atom::Uniform(_))) =>
            {
                Some(*when)
            }
            _ => None,
        })
        .expect("the fixture supplies a per-lane condition");
    st.wide.clear();
    st.exits = vec![
        LaneExit::Jump(1),
        LaneExit::Jump(2),
        LaneExit::Branch {
            when,
            taken: 1,
            not_taken: 3,
        },
        LaneExit::End,
    ];
    st.wave_any = vec![None, None, Some(true), None];
    assert_eq!(lower::branches_around_with_loop_exits(&st, &[2], false), Some(vec![]));
    assert_eq!(lower::branches_around_with_loop_exits(&st, &[2], true), Some(vec![2]));
    assert_eq!(lower::branches_around_with_loop_exits(&st, &[3], true), Some(vec![]));
}

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

const fn sop2(op: u32, sdst: u32, src0: u32, src1: u32) -> u32 {
    0x8000_0000 | op << 23 | sdst << 16 | src1 << 8 | src0
}

/// A branch by `imm` dwords from the next instruction (or `s_barrier`, `imm` 0).
const fn sopp(op: u32, imm: i16) -> u32 {
    0xBF80_0000 | op << 16 | imm as u16 as u32
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

const fn vop3(op: u32, dst: u32, src0: u32, src1: u32) -> [u32; 2] {
    [0xD400_0000 | op << 16 | dst, src0 | src1 << 9]
}

/// `buffer_store_dword v<data>, v<addr>, s[8:11], 0 offen offset:<offset>`
const fn store(data: u32, addr: u32, offset: u32) -> [u32; 2] {
    [0xE070_1000 | offset, 0x8002_0000 | data << 8 | addr]
}

/// `v<regs>` stored as each thread's dwords of the UAV, the thread's index (v0) times 16 on.
fn stores(regs: [u32; 4]) -> Vec<u32> {
    let mut w = vec![vop2(0x1A, 15, k(4), 0)]; // v_lshlrev_b32 v15, 4, v0
    for (n, r) in regs.into_iter().enumerate() {
        w.extend(store(r, 15, 4 * n as u32));
    }
    w
}

/// One console wave of 64 threads. Lane t goes round a loop `(t & s0) + 1` times, adding
/// `t & s1` to its sum each round; the loop is the compiler's usual one for lanes that leave
/// apart (a mask of the lanes still going round, the wave's branch back while some lane is
/// left). After it, over the lanes whose `t & s1` is not 0: their count (`s_bcnt1_i32_b64`),
/// the first of them (`v_readfirstlane_b32` of the thread index) and each one's number among
/// them (`v_mbcnt_lo/hi`). Every thread stores its sum, its number (0x55 where it has none),
/// the count and the first.
fn program() -> NativeProgram {
    loop_program(false)
}

fn loop_program(groups: bool) -> NativeProgram {
    let mut w = vec![
        vop2(0x1B, 4, 0, 0), // v_and_b32 v4, s0, v0: the lane's last round
        vop2(0x1B, 5, 1, 0), // v_and_b32 v5, s1, v0: what the lane adds a round
        vop1(0x01, 7, k(0)), // v_mov_b32 v7, 0
        vop1(0x01, 8, LIT),  // v_mov_b32 v8, 0x55
        0x55,
        sop1(0x03, 6, k(0)),      // s_mov_b32 s6, 0: the round
        sop1(0x04, 2, EXEC),      // s_mov_b64 s[2:3], exec
        sop1(0x04, 12, EXEC),     // s_mov_b64 s[12:13], exec: the lanes still going round
        sop1(0x04, 14, EXEC),     // s_mov_b64 s[14:15], exec
        sop2(0x0F, EXEC, 14, 12), // loop: s_and_b64 exec, s[14:15], s[12:13]
        vop2(0x25, 7, v(5), 7),   // v_add_nc_u32 v7, v5, v7
        sop1(0x04, EXEC, 14),     // s_mov_b64 exec, s[14:15]
        sop2(0x02, 6, 6, k(1)),   // s_add_i32 s6, s6, 1
    ];
    w.extend(vop3(0xC4, 14, 6, v(4))); // v_cmp_gt_u32 s[14:15], s6, v4: the lanes that are done
    w.extend([
        sop1(0x24, 14, 14),       // s_and_saveexec_b64 s[14:15], s[14:15]
        sop2(0x15, 12, 12, EXEC), // s_andn2_b64 s[12:13], s[12:13], exec
        sopp(0x05, -9),           // s_cbranch_scc1 loop
        sop1(0x04, EXEC, 2),      // s_mov_b64 exec, s[2:3]
        vopc(0xD5, k(0), 5),      // v_cmpx_ne_u32 exec, 0, v5
        sop1(0x10, 7, EXEC),      // s_bcnt1_i32_b64 s7, exec
        vop1(0x02, 16, v(0)),     // v_readfirstlane_b32 s16, v0
    ]);
    w.extend(vop3(0x365, 8, EXEC, k(0))); // v_mbcnt_lo_u32_b32 v8, exec_lo, 0
    w.extend(vop3(0x366, 8, EXEC + 1, v(8))); // v_mbcnt_hi_u32_b32 v8, exec_hi, v8
    w.extend([
        sop1(0x04, EXEC, 2), // s_mov_b64 exec, s[2:3]
        vop1(0x01, 9, 7),    // v_mov_b32 v9, s7
        vop1(0x01, 10, 16),  // v_mov_b32 v10, s16
    ]);
    let mut native = if groups {
        w.extend([
            vop1(0x01, 11, 17), // group ID x
            vop2(0x1a, 11, k(7), 11),
            vop2(0x25, 15, v(11), 0),
            vop2(0x1a, 15, k(4), 15),
        ]);
        for (n, reg) in [7, 8, 9, 10].into_iter().enumerate() {
            w.extend(store(reg, 15, 4 * n as u32));
        }
        native(&w)
    } else {
        w.extend(stores([7, 8, 9, 10]));
        return native(&w);
    };
    let StageIo::Cs(cs) = &mut native.io else {
        unreachable!()
    };
    cs.rsrc2 = (17 << 1) | (1 << 7);
    cs.threads = [128, 1, 1];
    native
}

/// What the console's 64 lanes store with the root constants `s0` and `s1`.
fn console(threads: u32, s0: u32, s1: u32) -> Vec<u32> {
    (0..threads)
        .flat_map(|t| {
            let first = t / 64 * 64;
            let counted: Vec<u32> = (first..first + 64).filter(|t| t & s1 != 0).collect();
            let number = if t & s1 != 0 {
                counted.iter().filter(|&&u| u < t).count() as u32
            } else {
                0x55
            };
            [
                (t & s1) * ((t & s0) + 1),
                number,
                counted.len() as u32,
                counted[0],
            ]
        })
        .collect()
}

// The hardware fixtures of fxceless/victory-fork pull request #1397 (ByFxbian), each run
// on the card twice: as the production lowering gives it, and in the paired form
// (`with_wave_pairs`, the program listed as `VICTORY_WAVE_PAIRS_ONLY` lists it). Both must
// store what the console's 64 lanes store, so the same. Not run without a physical
// 32-lane NVIDIA device and dxil.dll. `WAVE64_TEST_BENCH=1` also times each form (256
// dispatches, median of 10).

#[test]
fn nvidia_after_divergent_loop_matches_cpu() {
    let cases = [(0, 1), (3, 1), (7, 5), (24, 63), (7, 32), (7, 16), (63, 9)];
    both(&program(), &cases, |s0, s1| console(64, s0, s1), Plain::Translates);
}

#[test]
fn nvidia_two_waves_progress_independently() {
    let mut native = program();
    let StageIo::Cs(cs) = &mut native.io else {
        unreachable!()
    };
    cs.threads = [128, 1, 1];
    // Bit 6 distinguishes the two waves' trip counts and masks.
    both(
        &native,
        &[(127, 65), (63, 65), (7, 32), (7, 16)],
        |s0, s1| console(128, s0, s1),
        Plain::Translates,
    );
}

/// 257 groups of 128 threads, each storing at its group's place (the group id from s17).
#[test]
fn nvidia_many_wave64_groups_match_cpu() {
    let native = loop_program(true);
    let want = |s0, s1| console(128, s0, s1).repeat(257);
    let groups = [257, 1, 1];
    let Some(plain) = run(&native, &[(127, 65)], &want, groups, false).expect("the production lowering") else {
        return;
    };
    let paired = crate::emit::with_wave_pairs(true, || run(&native, &[(127, 65)], &want, groups, true));
    match paired {
        Ok(Some(paired)) => assert_eq!(plain, paired),
        Ok(None) => {}
        // (a group of two waves with a loop: no rendezvous for them in the paired form)
        Err(e) => eprintln!("paired form refused: {e}"),
    }
}

#[test]
fn nvidia_lds_between_wave_halves_matches_cpu() {
    execute_lds(64);
}

#[test]
fn nvidia_lds_between_two_waves_matches_cpu() {
    execute_lds(128);
}

#[test]
fn nvidia_lds_across_console_waves_matches_cpu() {
    let mut body = vec![
        vop2(0x1a, 1, k(2), 0),
        vop2(0x25, 2, k(1), 0),
        0xd834_0000,
        0x0000_0201,
        sopp(0x0a, 0),
        vop2(0x1d, 3, k(64), 0),
        vop2(0x1a, 3, k(2), 3),
        0xd8d8_0000,
        0x0400_0003,
    ];
    body.extend(stores([4, 2, 0, 0]));
    let mut native = native(&body);
    if let StageIo::Cs(cs) = &mut native.io {
        cs.threads = [128, 1, 1];
        cs.rsrc2 |= 1 << 15;
    }
    both(
        &native,
        &[(0, 0)],
        |_, _| (0..128).flat_map(|t| [(t ^ 64) + 1, t + 1, t, t]).collect(),
        Plain::MayRefuse,
    );
}

fn execute_lds(threads: u32) {
    let words = [
        vop2(0x1a, 1, k(2), 0), // v1 = byte address of this lane
        vop2(0x25, 2, k(1), 0), // v2 = thread index + 1
        0xd834_0000,
        0x0000_0201,             // ds_write_b32 v1, v2
        vop2(0x1d, 3, k(32), 0), // v3 = partner lane
        vop2(0x1a, 3, k(2), 3),
        0xd8d8_0000,
        0x0400_0003, // ds_read_b32 v4, v3, no explicit s_barrier
    ];
    let mut body = words.to_vec();
    body.extend(vop3(0x365, 5, 193, k(0))); // v_mbcnt_lo
    body.extend(vop3(0x366, 5, 193, v(5))); // v_mbcnt_hi
    body.extend(stores([4, 5, 2, 0]));
    let mut native = native(&body);
    if let StageIo::Cs(cs) = &mut native.io {
        cs.threads = [threads, 1, 1];
        cs.rsrc2 |= 1 << 15; // one 512-byte LDS unit
    }
    both(
        &native,
        &[(0, 0)],
        |_, _| {
            (0..threads)
                .flat_map(|lane| [(lane ^ 32) + 1, lane % 64, lane + 1, lane])
                .collect()
        },
        Plain::Translates,
    );
}

#[test]
fn nvidia_water_lds_pair_exchange() {
    let mut body = vec![
        vop2(0x1a, 1, k(3), 0),
        vop2(0x25, 2, k(1), 0),
        vop2(0x25, 3, k(64), 0),
        0xd838_0100, 0x0003_0201,
        vop2(0x1d, 4, k(32), 0),
        vop2(0x1a, 4, k(3), 4),
        0xd8dc_0001, 0x0700_0004,
    ];
    body.extend(stores([7, 8, 2, 3]));
    let mut native = native(&body);
    if let StageIo::Cs(cs) = &mut native.io {
        cs.rsrc2 |= 1 << 15;
    }
    both(
        &native,
        &[(0, 0)],
        |_, _| (0..64).flat_map(|lane| [(lane ^ 32) + 64, (lane ^ 32) + 1, lane + 1, lane + 64]).collect(),
        Plain::MayRefuse,
    );
}

#[test]
fn nvidia_water_lds_masked_min_max() {
    let mut body = vec![
        vop2(0x1a, 1, k(3), 0),
        vop1(0x01, 2, k(0)),
        vop1(0x01, 3, 193),
        0xd838_0100, 0x0003_0201,
        vop1(0x01, 4, k(0)),
        vop2(0x25, 5, k(1), 0),
        vop2(0x1d, 6, 1, 0),
        sop1(0x04, 2, EXEC),
        vopc(0xd4, 0, 6),
        sopp(0x08, 4),
        0xd81c_0004, 0x0000_0504,
        0xd820_0000, 0x0000_0504,
        sop1(0x04, EXEC, 2),
        0xd8dc_0100, 0x0700_0004,
    ];
    body.extend(stores([7, 8, 0, 5]));
    let mut native = native(&body);
    if let StageIo::Cs(cs) = &mut native.io {
        cs.rsrc2 |= 1 << 15;
    }
    both(
        &native,
        &[(0, 0), (1, 0), (31, 0), (32, 32), (33, 32), (64, 0)],
        |limit, xor| {
            let active = (0..64).filter(|lane| (lane ^ xor) < limit).map(|lane| lane + 1);
            let max = active.clone().max().unwrap_or(0);
            let min = active.min().unwrap_or(u32::MAX);
            (0..64).flat_map(|lane| [max, min, lane, lane + 1]).collect()
        },
        Plain::MayRefuse,
    );
}

#[test]
fn nvidia_wave_reduction_and_lane_reads_match_cpu() {
    // The low half contributes bit 0 and the high half bit 1.
    let mut body = vec![vop2(0x16, 7, k(5), 0), vop2(0x25, 7, k(1), 7)];
    body.extend(super::super::wave::WAVE_OR_V7_TO_S7);
    body.extend(vop3(0x360, 16, v(0), k(37))); // v_readlane_b32 s16, v0, 37
    body.extend([vop1(0x01, 9, 7), vop1(0x01, 10, 16)]);
    body.extend(stores([9, 10, 0, 0]));
    both(
        &native(&body),
        &[(0, 0)],
        |_, _| (0..64).flat_map(|lane| [3, 37, lane, lane]).collect(),
        Plain::Translates,
    );
}

#[test]
fn nvidia_paired_permute_obeys_exec() {
    let mut body = vec![
        sop1(0x04, 2, EXEC),
        vop2(0x25, 2, k(1), 0),  // v2 = lane + 1
        vop2(0x1d, 3, k(32), 0), // v3 = partner lane
        vop2(0x1a, 3, k(2), 3),
        vop1(0x01, 4, k(17)),
        vopc(0xd4, 0, 0), // v_cmpx_gt_u32 exec, s0, v0
        0xdacc_0000,
        0x0400_0203, // ds_bpermute_b32 v4, v3, v2
        sop1(0x04, EXEC, 2),
    ];
    body.extend(stores([4, 0, 2, 0]));
    both(
        &native(&body),
        &[(1, 0), (32, 0), (33, 0), (64, 0)],
        |limit, _| {
            (0..64)
                .flat_map(|lane| {
                    let partner = lane ^ 32;
                    let value = if lane >= limit {
                        17
                    } else if partner >= limit {
                        0
                    } else {
                        partner + 1
                    };
                    [value, lane, lane + 1, lane]
                })
                .collect()
        },
        Plain::MayRefuse,
    );
}

fn fp32_reduce(op: u32, identity: u32) -> Vec<u32> {
    let mut words = super::super::wave::WAVE_OR_V7_TO_S7.to_vec();
    if identity != 0 {
        words[2] = (words[2] & !0x1ff) | LIT;
        words.insert(3, identity);
    }
    for w in &mut words {
        if *w >> 25 == 28 {
            *w = (*w & 0x01ff_ffff) | op << 25;
        }
    }
    words
}

fn fp32_program(op: u32, identity: u32, other: [u32; 3], lds: bool) -> NativeProgram {
    let mut body = vec![vop1(1, 7, 1), vop2(0x1b, 4, k(3), 0)];
    for (i, bits) in other.into_iter().enumerate() {
        body.extend([
            vopc(0xc5, k(i as u32 + 1), 4), // v_cmp_ne_u32 vcc, i+1, v4
            vop2(1, 7, LIT, 7),
            bits, // v_cndmask_b32 v7, bits, v7, vcc
        ]);
    }
    if lds {
        body.extend([
            vop2(0x1a, 2, k(2), 0),
            0xd834_0000,
            0x0000_0702, // ds_write_b32 v2, v7
            vop2(0x1d, 3, k(32), 0),
            vop2(0x1a, 3, k(2), 3),
        ]);
    }
    body.extend([
        sop1(4, 12, EXEC),
        vop2(0x1d, 5, k(45), 0), // sparse EXEC across either half
        vopc(0xd4, 0, 5),
    ]);
    if lds {
        body.extend([0xd8d8_0000, 0x0700_0003]); // ds_read_b32 v7, v3, under EXEC
    }
    body.extend(fp32_reduce(op, identity));
    body.extend([sop1(4, EXEC, 12), vop1(1, 9, 7)]);
    body.extend(stores([9, 0, 0, 0]));
    let mut program = native(&body);
    if lds && let StageIo::Cs(cs) = &mut program.io {
        cs.rsrc2 |= 1 << 15;
    }
    program
}

fn fp32_reference(op: u32, a: u32, b: u32) -> u32 {
    let flush = |bits| {
        if bits & 0x7fff_ffff < 0x0080_0000 {
            bits & 0x8000_0000
        } else {
            bits
        }
    };
    let (a, b) = (flush(a), flush(b));
    let (x, y) = (f32::from_bits(a), f32::from_bits(b));
    if op == 3 {
        if x.is_nan() {
            return a;
        }
        if y.is_nan() {
            return b;
        }
        if x.is_infinite() && y == -x {
            return 0xffc0_0000;
        }
        return flush((x + y).to_bits());
    }
    if y.is_nan() {
        return a;
    }
    if x.is_nan() {
        return b;
    }
    if x == 0.0 && y == 0.0 {
        return if op == 15 { a | b } else { a & b };
    }
    if (op == 15 && x < y) || (op == 16 && x > y) {
        a
    } else {
        b
    }
}

fn fp32_expected(op: u32, ident: u32, values: [u32; 4], active: u32, lds: bool) -> Vec<u32> {
    let mut lanes: [u32; 64] = std::array::from_fn(|lane| {
        if (lane as u32 ^ 45) < active {
            values[(if lds { lane ^ 32 } else { lane }) & 3]
        } else {
            ident
        }
    });
    for (width, indices) in [
        (8, &[1, 0, 3, 2, 5, 4, 7, 6][..]),
        (8, &[2, 2, 0, 0, 6, 6, 4, 4][..]),
        (8, &[4, 4, 4, 4, 0, 0, 0, 0][..]),
    ] {
        let old = lanes;
        lanes = std::array::from_fn(|i| {
            fp32_reference(op, old[i / width * width + indices[i % width]], old[i])
        });
    }
    let old = lanes;
    lanes = std::array::from_fn(|i| fp32_reference(op, old[i ^ 15], old[i]));
    let old = lanes;
    lanes = std::array::from_fn(|i| fp32_reference(op, old[(i / 16 ^ 1) * 16], old[i]));
    let result = fp32_reference(op, lanes[32], lanes[0]);
    (0..64)
        .flat_map(|lane| [result, lane, lane, lane])
        .collect()
}

#[test]
#[ignore = "requires NVIDIA Wave32 hardware and VICTORY_WAVE_PAIRS_FP32=non-ieee-rne-ftz"]
fn nvidia_paired_fp32_ordered_sum() {
    let other = [
        1.0f32.to_bits(),
        (-16_777_216.0f32).to_bits(),
        1.0f32.to_bits(),
    ];
    let values = [16_777_216.0f32.to_bits(), other[0], other[1], other[2]];
    let linear = values
        .into_iter()
        .cycle()
        .take(64)
        .fold(0, |sum, term| fp32_reference(3, sum, term));
    assert_ne!(fp32_expected(3, 0, values, 64, false)[0], linear);
    let cases = [
        (0, 0),
        (32, 16_777_216.0f32.to_bits()),
        (33, 16_777_216.0f32.to_bits()),
        (64, 16_777_216.0f32.to_bits()),
        (64, 0x7f80_0021),
        (19, 0x7fc0_0011),
    ];
    execute_wave32(&fp32_program(3, 0, other, false), &cases, |n, src| {
        fp32_expected(3, 0, [src, other[0], other[1], other[2]], n, false)
    });
    let other = [0x8000_0000; 3];
    execute_wave32(
        &fp32_program(3, 0x8000_0000, other, false),
        &[(1, 0x8000_0001), (64, 0x8000_0000)],
        |n, src| {
            fp32_expected(
                3,
                0x8000_0000,
                [src, other[0], other[1], other[2]],
                n,
                false,
            )
        },
    );
    let other = [0xff80_0000, 0x0080_0001, 0x8080_0000];
    execute_wave32(
        &fp32_program(3, 0, other, true),
        &[(7, 0x7f80_0000), (64, 0x7f80_0000), (64, 0x0080_0000)],
        |n, src| fp32_expected(3, 0, [src, other[0], other[1], other[2]], n, true),
    );
    let other = [0, 0x8080_0000, 0x8000_0000];
    execute_wave32(
        &fp32_program(3, 0, other, false),
        &[(64, 0x0080_0001), (64, 0x0080_0002)],
        |n, src| fp32_expected(3, 0, [src, other[0], other[1], other[2]], n, false),
    );
    let other = [0x8080_0001, 0x0080_0000, 0x8080_0001];
    execute_wave32(
        &fp32_program(3, 0, other, false),
        &[(64, 0x0080_0000)],
        |n, src| {
            let expected = fp32_expected(3, 0, [src, other[0], other[1], other[2]], n, false);
            assert_eq!(expected[0], 0x8000_0000);
            expected
        },
    );
}

#[test]
#[ignore = "requires NVIDIA Wave32 hardware and VICTORY_WAVE_PAIRS_FP32=non-ieee-rne-ftz"]
fn nvidia_paired_fp32_min_max() {
    for (op, ident) in [(15, 0x7f80_0000), (16, 0xff80_0000)] {
        for other in [
            [0, 0x8000_0000, 0x8000_0001],
            [0x7fc0_0001, 0x7f80_0002, 0xffc0_0003],
        ] {
            let cases = [
                (0, 0),
                (17, 0x7fc0_0011),
                (32, 0x7f80_0000),
                (64, 0x7f80_0011),
                (64, 1),
            ];
            execute_wave32(&fp32_program(op, ident, other, false), &cases, |n, src| {
                fp32_expected(op, ident, [src, other[0], other[1], other[2]], n, false)
            });
        }
    }
}

#[test]
fn paired_fp32_refuses_an_unconfirmed_guest_mode() {
    const TEST: &str = "WAVE64_FP32_MODE_TEST";
    if std::env::var_os(TEST).is_some() {
        let program = fp32_program(3, 0, [0; 3], false);
        let p = crate::spill::build(&program).unwrap();
        let bytes = crate::dxbc::build(&[]);
        let c = crate::dxbc::Container::parse(&bytes).unwrap();
        let old = lower::TEST_HOST_WAVE.replace(Some(32));
        let result = crate::emit::paired(&c, &program, &p);
        lower::TEST_HOST_WAVE.set(old);
        assert!(matches!(
            result,
            Err(ShaderError::Unsupported {
                what: "paired FP32 reduction requires a confirmed non-IEEE RNE/FTZ guest mode",
                ..
            })
        ));
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "paired_fp32_refuses_an_unconfirmed_guest_mode",
            "--nocapture",
        ])
        .env(TEST, "1")
        .env_remove("VICTORY_WAVE_PAIRS_FP32")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// `native` in the paired form on a 32-lane NVIDIA card, checked against `expected` for each
/// case (fork pull request #1419's helper, on this file's `run`).
fn execute_wave32(native: &NativeProgram, cases: &[(u32, u32)], expected: impl Fn(u32, u32) -> Vec<u32>) {
    crate::emit::with_wave_pairs(true, || run(native, cases, &expected, [1, 1, 1], true))
        .unwrap_or_else(|e| panic!("paired form: {e}"));
}

/// The water's whole-wave sums as the console compiler expands them (the transmission
/// volumetrics programs 5dc3b25986622949 and 6f225b641c5b0708): two FP32 sums in a row, the
/// first leaving its partial sums in v1 and v2, which the second clears in every lane
/// (`v_cndmask_b32 v1, 0, v7, s[4:5]`, `v_permlanex16_b32 v2, v1, 0, 0`); then the 64-bit
/// float count of the lanes (a sum of `term`, 1.0 in the water: `None` a uniform that is not
/// a constant). `seen`: the second sum's partials are read afterwards.
fn water_sums(seen: bool, term: Option<u32>) -> NativeProgram {
    let mut body = vec![
        vop1(1, 7, 1), // v_mov_b32 v7, s1
        sop1(4, 12, EXEC),
        vop2(0x1d, 5, k(45), 0), // sparse EXEC across either half
        vopc(0xd4, 0, 5),
    ];
    body.extend(fp32_reduce(3, 0));
    body.push(vop1(1, 8, 7)); // v_mov_b32 v8, s7: the first sum
    body.extend(fp32_reduce(3, 0));
    body.push(vop1(1, 10, k(0))); // v_mov_b32 v10, 0
    match term {
        Some(hi) => body.extend([vop1(1, 11, LIT), hi]),
        None => body.push(vop1(1, 11, 2)), // v_mov_b32 v11, s2
    }
    body.extend([
        0xBE92_25C1, // s_or_saveexec_b64 s[18:19], -1
        0x7E04_0280, // v_mov_b32 v2, 0
        0x7E06_0280, // v_mov_b32 v3, 0
        0xD501_0002,
        0x004A_1502, // v_cndmask_b32 v2, v2, v10, s[18:19]
        0xD501_0003,
        0x004A_1703, // v_cndmask_b32 v3, v3, v11, s[18:19]
    ]);
    for dpp in [0xDE54_C100, 0x9360_1200, 0x0009_2400] {
        body.extend([
            0x7E08_02E9,
            dpp | 2, // v_mov_b32 v4, v2 dpp8:[...]
            0x7E0A_02E9,
            dpp | 3, // v_mov_b32 v5, v3 dpp8:[...]
            0xD564_0002,
            0x0002_0504, // v_add_f64 v[2:3], v[4:5], v[2:3]
        ]);
    }
    body.extend([
        0x7E08_02FA,
        0xFF09_4002, // v_mov_b32 v4, v2 row_mirror bound_ctrl:1
        0x7E0A_02FA,
        0xFF09_4003, // v_mov_b32 v5, v3 row_mirror bound_ctrl:1
        0xD564_0002,
        0x0002_0504,
        0xD778_0004,
        0x0201_0102, // v_permlanex16_b32 v4, v2, 0, 0
        0xD778_0005,
        0x0201_0103, // v_permlanex16_b32 v5, v3, 0, 0
        0xD564_0002,
        0x0002_0504,
        0xD760_0010,
        0x0001_4102, // v_readlane_b32 s16, v2, 32
        0xD760_0011,
        0x0001_4103, // v_readlane_b32 s17, v3, 32
        0xD564_0002,
        0x0002_0410, // v_add_f64 v[2:3], s[16:17], v[2:3]
        0xD760_0010,
        0x0001_0102, // v_readlane_b32 s16, v2, 0
        0xD760_0011,
        0x0001_0103, // v_readlane_b32 s17, v3, 0
        sop1(3, 14, 16),
        sop1(3, 15, 17),
        sop1(4, EXEC, 18), // s_mov_b64 exec, s[18:19]
        sop1(4, EXEC, 12),
        vop1(1, 9, 7),   // the second sum
        vop1(1, 10, 14), // the count's low dword
        vop1(1, 11, if seen { v(1) } else { 15 }),
    ]);
    body.extend(stores([8, 9, 10, 11]));
    native(&body)
}

#[test]
fn paired_fp32_takes_the_water_sums() {
    const TEST: &str = "WAVE64_FP32_WATER_TEST";
    if std::env::var_os(TEST).is_some() {
        let bytes = crate::dxbc::build(&[]);
        let c = crate::dxbc::Container::parse(&bytes).unwrap();
        let translate = |program: NativeProgram| {
            let p = crate::spill::build(&program).unwrap();
            let old = lower::TEST_HOST_WAVE.replace(Some(32));
            let r = crate::emit::paired(&c, &program, &p).map(|(_, wave)| wave);
            lower::TEST_HOST_WAVE.set(old);
            r
        };
        let one = Some(0x3FF0_0000);
        assert!(matches!(translate(water_sums(false, one)), Ok(Some(32))));
        // -1.0 and 0.0 count too
        assert!(matches!(
            translate(water_sums(false, Some(0xBFF0_0000))),
            Ok(Some(32))
        ));
        assert!(matches!(
            translate(water_sums(false, Some(0))),
            Ok(Some(32))
        ));
        // a partial sum read afterwards is no full reduction
        assert!(matches!(
            translate(water_sums(true, one)),
            Err(ShaderError::Unsupported {
                what: "a paired FP32 section without a proven full reduction",
                ..
            })
        ));
        // 64-bit float sums that may round (2^1023 overflows), or of a term not known: refused
        for term in [Some(0x7FE0_0000), None] {
            assert!(matches!(
                translate(water_sums(false, term)),
                Err(ShaderError::Unsupported {
                    what: "a paired FP64 reduction",
                    ..
                })
            ));
        }
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["paired_fp32_takes_the_water_sums", "--nocapture"])
        .env(TEST, "1")
        .env("VICTORY_WAVE_PAIRS_FP32", "non-ieee-rne-ftz")
        // (the default passes, fast math among them: the paired program is written without)
        .env_remove("VICTORY_SHADER_OPT")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// The water tile programs' LDS use (5dc3b25986622949 at 0x21c..0x4c4): thread 0 alone stores
/// the tile's starting maximum (0, dword 0) and minimum (-1, dword 1), then every thread
/// `ds_max_u32`s and `ds_min_u32`s its key in and reads both back. `max_start` replaces the 0;
/// `stray`: a write at an address that is not a constant; `atomics: false` leaves them out.
fn tile_min_max(max_start: u32, stray: bool, atomics: bool) -> NativeProgram {
    let mut body = vec![
        sop1(4, 12, EXEC),     // s_mov_b64 s[12:13], exec
        vopc(0xd2, k(0), 0),   // v_cmpx_eq_u32 exec, 0, v0
        vop1(1, 3, LIT),
        max_start,             // v_mov_b32 v3, max_start
        vop1(1, 4, k(65)),     // v_mov_b32 v4, -1
        vop1(1, 5, k(0)),      // v_mov_b32 v5, 0
        0xD838_0100,
        0x0004_0305,           // ds_write2_b32 v5, v3, v4 offset1:1
        sop1(4, EXEC, 12),     // s_mov_b64 exec, s[12:13]
        vop1(1, 5, k(0)),      // v_mov_b32 v5, 0
    ];
    if stray {
        body.extend([0xD834_0000, 0x0000_0000]); // ds_write_b32 v0, v0
    }
    if atomics {
        body.extend([
            0xD81C_0004,
            0x0000_0005, // ds_min_u32 v5, v0 offset:4
            0xD820_0000,
            0x0000_0005, // ds_max_u32 v5, v0
        ]);
    }
    body.extend([0xD8DC_0100, 0x0600_0005]); // ds_read2_b32 v[6:7], v5 offset1:1
    body.extend(stores([6, 7, 0, 0]));
    let mut program = native(&body);
    if let StageIo::Cs(cs) = &mut program.io {
        cs.rsrc2 |= 1 << 15;
    }
    program
}

#[test]
fn accumulator_inits_are_hoisted_to_the_start() {
    let hoists = |program: NativeProgram| {
        let p = crate::spill::build(&program).unwrap();
        lower::lds_init_hoists(&p, 128)
    };
    assert_eq!(hoists(tile_min_max(0, false, true)), [(0, 0), (1, u32::MAX)]);
    // a start that is not the maximum's identity stays where it is
    assert_eq!(hoists(tile_min_max(5, false, true)), [(1, u32::MAX)]);
    // plain stores (a flag, data) are not accumulators
    assert_eq!(hoists(tile_min_max(0, false, false)), []);
    // a write that could land anywhere: nothing is known
    assert_eq!(hoists(tile_min_max(0, true, true)), []);
    // the program still translates, both ways
    let bytes = crate::dxbc::build(&[]);
    let c = crate::dxbc::Container::parse(&bytes).unwrap();
    let program = tile_min_max(0, false, true);
    let p = crate::spill::build(&program).unwrap();
    let old = lower::TEST_HOST_WAVE.replace(Some(32));
    let paired = crate::emit::paired(&c, &program, &p);
    lower::TEST_HOST_WAVE.set(old);
    let (shader, wave) = paired.unwrap();
    assert_eq!(wave, Some(32));
    if let Ok(mut v) = super::super::validator::Validator::load(
        super::super::validator::Validator::default_path().as_deref(),
    ) {
        let mut dxil = shader.dxil.clone();
        v.validate(&mut dxil).unwrap();
    }
}

#[test]
fn paired_permute_rejects_gds() {
    let mut body = vec![vop1(0x01, 2, k(11)), 0xdace_0000, 0x0100_0200];
    body.extend(stores([1, 0, 2, 0]));
    let native = native(&body);
    let p = crate::spill::build(&native).unwrap();
    let bytes = crate::dxbc::build(&[]);
    let c = crate::dxbc::Container::parse(&bytes).unwrap();
    let old = lower::TEST_HOST_WAVE.replace(Some(32));
    let result = crate::emit::paired(&c, &native, &p);
    lower::TEST_HOST_WAVE.set(old);
    assert!(matches!(
        result,
        Err(ShaderError::Unsupported { what: "GDS", .. })
    ));
}

#[test]
fn paired_lds_respects_host_shared_memory_limit() {
    let mut body = vec![
        vop2(0x1a, 1, k(2), 0),
        0xd834_0000,
        0x0000_0001, // ds_write_b32 v1, v0
    ];
    body.extend(stores([0; 4]));
    let bytes = crate::dxbc::build(&[]);
    let c = crate::dxbc::Container::parse(&bytes).unwrap();
    let old = lower::TEST_HOST_WAVE.replace(Some(32));
    for (threads, lds_units, allowed) in [(64, 64, true), (64, 65, false), (128, 64, false)] {
        let mut native = native(&body);
        let StageIo::Cs(cs) = &mut native.io else {
            unreachable!()
        };
        cs.threads = [threads, 1, 1];
        cs.rsrc2 |= lds_units << 15;
        let p = crate::spill::build(&native).unwrap();
        assert_eq!(
            crate::emit::paired(&c, &native, &p).is_ok(),
            allowed,
            "{threads} threads, {lds_units} LDS units"
        );
    }
    lower::TEST_HOST_WAVE.set(old);
}

/// Whether the production lowering (not paired) must translate a fixture: some are there
/// for the paired form's LDS and permutes, which the other forms may refuse.
#[derive(Clone, Copy, PartialEq)]
enum Plain {
    Translates,
    MayRefuse,
}

/// `native` run on the card for each case, as the production lowering gives it and paired;
/// both store what `expected` says, so the same. With `VICTORY_WAVE_PAIRS=1` the first run
/// may be paired too.
fn both(native: &NativeProgram, cases: &[(u32, u32)], expected: impl Fn(u32, u32) -> Vec<u32>, plain: Plain) {
    let first = match run(native, cases, &expected, [1, 1, 1], false) {
        Ok(None) => return,
        Ok(Some(got)) => Some(got),
        Err(e) if plain == Plain::MayRefuse => {
            eprintln!("the production lowering refuses it ({e}): only the paired form runs");
            None
        }
        Err(e) => panic!("production lowering: {e}"),
    };
    let paired = crate::emit::with_wave_pairs(true, || run(native, cases, &expected, [1, 1, 1], true))
        .unwrap_or_else(|e| panic!("paired form: {e}"))
        .expect("the card ran the first form");
    if let Some(first) = first {
        assert_eq!(first, paired, "the two forms stored different words");
        eprintln!("paired and unpaired: the same {} words", paired.iter().map(Vec::len).sum::<usize>());
    }
}

/// `native` translated for a 32-lane host and run on the card with each case's root
/// constants `s0`, `s1`: the UAV's words for each case, checked against `expected`. `paired`:
/// the translation must be the paired form (half the console's threads per group). None
/// without a physical 32-lane NVIDIA device or dxil.dll; Err if it does not translate.
fn run(
    native: &NativeProgram,
    cases: &[(u32, u32)],
    expected: &impl Fn(u32, u32) -> Vec<u32>,
    groups: [u32; 3],
    paired: bool,
) -> Result<Option<Vec<Vec<u32>>>, String> {
    let _one = crate::emit::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::*;
    let Ok(host) = Host::new(false) else {
        eprintln!("no SM 6.6 hardware device; not run");
        return Ok(None);
    };
    let mut caps = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
    // SAFETY: the query writes the options structure of the supplied size; the factory
    // enumerates the adapter for this device's LUID.
    let desc = unsafe {
        host.device
            .CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS1, (&raw mut caps).cast(), size_of_val(&caps) as u32)
            .map_err(|e| e.to_string())?;
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).map_err(|e| e.to_string())?;
        factory
            .EnumAdapterByLuid::<IDXGIAdapter1>(host.device.GetAdapterLuid())
            .and_then(|a| a.GetDesc1())
            .map_err(|e| e.to_string())?
    };
    if desc.VendorId != 0x10de || desc.Flags & 2 != 0 || caps.WaveLaneCountMax != 32 {
        eprintln!("not a physical 32-lane NVIDIA device; not run");
        return Ok(None);
    }
    let Ok(mut validator) = validator::Validator::load(validator::Validator::default_path().as_deref()) else {
        eprintln!("no dxil.dll; not run");
        return Ok(None);
    };
    let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
    eprintln!("adapter: {}", String::from_utf16_lossy(&desc.Description[..end]));
    let p = crate::spill::build(native).expect("build native shader");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resources");
    let io = io::raise(&p, native).expect("I/O");
    let bytes = crate::dxbc::build(&[]);
    let c = crate::dxbc::Container::parse(&bytes).unwrap();
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let result = crate::emit::emit(&c, native, &p, &st, &io, &res);
    let key = crate::emit::translation_fingerprint();
    lower::TEST_HOST_WAVE.set(was);
    // (the global switch is the fingerprint's last byte: the shader cache keeps the forms apart)
    let global_pairs = std::env::var_os("VICTORY_WAVE_PAIRS").is_some_and(|v| v == "1");
    assert_eq!(key.last(), Some(&u8::from(global_pairs)), "shader cache translation mode");
    let mut shader = result.map_err(|e| e.to_string())?;
    let StageIo::Cs(cs) = native.io else {
        unreachable!()
    };
    let threads = cs.threads.iter().product::<u32>();
    let container = crate::dxbc::Container::parse(&shader.dxil).unwrap();
    let psv = container.part(crate::dxbc::FourCC::PSV0).unwrap();
    let dim = [40, 44, 48].map(|at| u32::from_le_bytes(psv.data[at..at + 4].try_into().unwrap()));
    let is_paired = dim == [threads / 2, 1, 1];
    if paired {
        assert!(is_paired, "the paired form was asked for: {dim:?} threads for a group of {threads}");
    } else if !global_pairs {
        assert_eq!(dim, [threads, 1, 1]);
    }
    eprintln!("paired={is_paired} threads={dim:?} groups={groups:?}");
    validator.validate(&mut shader.dxil).expect("DXIL validation");
    let (root, pso) = host.compute_pipeline(&shader).expect("compute pipeline");
    if let Some(dir) = std::env::var_os("WAVE64_TEST_DXIL_DIR") {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("test").rsplit("::").next().unwrap_or("test");
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}-paired{is_paired}.dxil")), &shader.dxil)
            .expect("export DXIL");
    }
    let timing = std::env::var_os("WAVE64_TEST_BENCH").is_some_and(|v| v == "1");
    let mut out = Vec::new();
    for (i, &(s0, s1)) in cases.iter().enumerate() {
        let mut ud = vec![0; shader.user_data_dwords().max(1) as usize];
        for u in &shader.user_data {
            if let (RootType::Constants, 0, d @ 0..=3) = (u.root, u.slot, u.dword) {
                ud[u.index as usize] = [s0, s1, 0, 0][d as usize];
            }
        }
        let words = threads * 4 * groups.iter().product::<u32>();
        let dispatch = Dispatch {
            roots: vec![(0, Root::Constants(ud))],
            views: &[View::RawUav(words)],
            samplers: &[],
            groups,
        };
        let mut want = expected(s0, s1);
        want.resize(words as usize, 0);
        let got = host.dispatch(&root, &pso, &dispatch).expect("dispatch");
        compare(&got[0], &want, s0, s1);
        if timing && i + 1 == cases.len() {
            let mut samples = Vec::with_capacity(10);
            for sample in 0..12 {
                let (again, ns) = host.dispatch_repeated(&root, &pso, &dispatch, 256).expect("timed dispatch");
                compare(&again[0], &want, s0, s1);
                if sample >= 2 {
                    samples.push(ns);
                }
            }
            samples.sort_by(f64::total_cmp);
            eprintln!(
                "paired={is_paired}: GPU ns/pass (256 dispatches with UAV barriers between, 2 warm-ups + 10 samples): median {:.1}, min {:.1}, max {:.1}",
                (samples[4] + samples[5]) / 2.0,
                samples[0],
                samples[9],
            );
        }
        out.push(got.into_iter().next().unwrap_or_default());
    }
    host.check_hardware_validation();
    eprintln!("32-lane hardware: validated {}-byte DXIL, {} CPU comparisons passed", shader.dxil.len(), cases.len());
    Ok(Some(out))
}

fn compare(got: &[u32], want: &[u32], s0: u32, s1: u32) {
    assert_eq!(got.len(), want.len());
    if let Some(i) = got.iter().zip(want).position(|(a, b)| a != b) {
        panic!(
            "word {i}: got {:#x}, expected {:#x} (s0={s0}, s1={s1})",
            got[i], want[i]
        );
    }
}

/// The water's 64-lane programs that a 32-lane host refuses in every exact form
/// (`water_shading:CS_TransmissionVolumetrics_Tiled`, `..._Tiled_LocalLights` and
/// `CS_TransmissionVolumetricsSubmerged`): with `VICTORY_WAVE_PAIRS_ONLY` naming them (the
/// paired form tried first, [`crate::emit::with_wave_pairs`]) and the confirmed guest float
/// mode (`VICTORY_WAVE_PAIRS_FP32=non-ieee-rne-ftz`), each translates to 32-lane waves.
///
/// Game-derived containers stay outside the repository: `VICTORY_WATER_SHADERS=<dir>` names
/// a directory whose files hold the hashes in their names (as `victory translate` writes them
/// with `VICTORY_DUMP_MATCH=<dir>;#<hash>|...`). Without it, or without a file, not run.
#[test]
fn water_programs_translate_paired() {
    const CHILD: &str = "WAVE64_WATER_PROGRAMS_CHILD";
    const WATER: [&str; 3] = ["5dc3b25986622949", "6f225b641c5b0708", "926c46bab3b86bd6"];
    let Some(dir) = std::env::var_os("VICTORY_WATER_SHADERS") else {
        eprintln!("VICTORY_WATER_SHADERS not set; not run");
        return;
    };
    if std::env::var_os(CHILD).is_none() {
        // (the switches are read from the environment: set in a child of its own)
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["water_programs_translate_paired", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("VICTORY_WAVE_PAIRS_FP32", "non-ieee-rne-ftz")
            .env_remove("VICTORY_WAVE_PAIRS")
            .output()
            .unwrap();
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{output:?}");
        return;
    }
    let files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .expect("VICTORY_WATER_SHADERS")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    let mut validator = validator::Validator::load(validator::Validator::default_path().as_deref()).ok();
    let mut failed = Vec::new();
    for hash in WATER {
        let Some(path) = files.iter().find(|f| f.to_string_lossy().contains(hash)) else {
            eprintln!("{hash}: no container in the directory; not run");
            continue;
        };
        let bytes = std::fs::read(path).unwrap();
        let old = lower::TEST_HOST_WAVE.replace(Some(32));
        let plain = crate::emit::translate(&bytes);
        let paired = crate::emit::with_wave_pairs(true, || crate::emit::translate(&bytes));
        lower::TEST_HOST_WAVE.set(old);
        eprintln!(
            "{hash}: exact forms {}",
            plain.as_ref().map_or_else(|e| format!("refused ({e})"), |_| "translate".into())
        );
        match paired {
            Ok(mut shader) => {
                let container = crate::dxbc::Container::parse(&shader.dxil).unwrap();
                let psv = container.part(crate::dxbc::FourCC::PSV0).unwrap();
                let dim = [40, 44, 48].map(|at| u32::from_le_bytes(psv.data[at..at + 4].try_into().unwrap()));
                // (the water's groups are 8 by 8: two console lanes a host thread)
                let host_threads = dim.iter().product::<u32>();
                let checked = match &mut validator {
                    Some(v) => v.validate(&mut shader.dxil).map(|_| "valid DXIL").unwrap_or_else(|e| {
                        failed.push(format!("{hash}: DXIL validation: {e}"));
                        "invalid DXIL"
                    }),
                    None => "no dxil.dll",
                };
                eprintln!("{hash}: paired form translates: {dim:?} host threads a group, {checked}");
                if host_threads != 32 {
                    failed.push(format!("{hash}: {dim:?} host threads, not the paired 32"));
                }
            }
            Err(e) => failed.push(format!("{hash}: paired form refused: {e}")),
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
