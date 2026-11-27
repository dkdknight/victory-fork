use super::*;

pub(super) fn lds_meetings(p: &crate::ir::Program, st: &Structured, all: &[bool]) -> Option<Vec<(bool, bool)>> {
    let n = st.exits.len();
    let succ = |b: usize| -> Vec<usize> {
        match st.exits[b] {
            LaneExit::Jump(t) => vec![t],
            LaneExit::Branch { taken, not_taken, .. } => vec![taken, not_taken],
            LaneExit::End | LaneExit::Indirect => Vec::new(),
        }
    };
    // regions: blocks not every lane runs, joined by the edges between them
    let mut region: Vec<usize> = (0..n).collect();
    fn find(region: &mut [usize], mut b: usize) -> usize {
        while region[b] != b {
            region[b] = region[region[b]];
            b = region[b];
        }
        b
    }
    for b in 0..n {
        for t in succ(b) {
            if !all[b] && !all[t] {
                let (x, y) = (find(&mut region, b), find(&mut region, t));
                region[x] = y;
            }
        }
    }
    // per region: LDS reads, LDS writes
    let mut reads = vec![false; n];
    let mut writes = vec![false; n];
    for (b, block) in p.blocks.iter().enumerate().take(n) {
        if all[b] {
            continue;
        }
        let r = find(&mut region, b);
        for i in &block.insts {
            let name = i.name();
            if !matches!(i.fields, Fields::Ds { .. }) || name.starts_with("ds_swizzle") || name.contains("permute") {
                continue;
            }
            if name.starts_with("ds_read") {
                reads[r] = true;
            } else {
                writes[r] = true;
            }
        }
    }
    if (0..n).any(|r| reads[r] && writes[r]) {
        return None;
    }
    let mut meet = vec![(false, false); n];
    for b in 0..n {
        for t in succ(b) {
            if all[b] && !all[t] {
                let r = find(&mut region, t);
                meet[b].1 |= reads[r] || writes[r];
            } else if !all[b] && all[t] {
                let r = find(&mut region, b);
                meet[t].0 |= reads[r] || writes[r];
            }
        }
    }
    Some(meet)
}

/// Masks held as numbers or constant lane sets in guards or lane branches: the lane tests its
/// own bit by its console lane.
pub(super) fn lane_constant_masks(st: &Structured) -> bool {
    let whens = st.exits.iter().filter_map(|e| match *e {
        LaneExit::Branch { when, .. } => Some(when),
        _ => None,
    });
    let mut work: Vec<Bool> = st.guards.iter().flatten().flatten().copied().chain(whens).collect();
    // Through lane-mask joins too: an edge that brings a number or a constant lane set needs
    // the lane's own bit of it.
    // shortcut: masks only selects read (`v_cndmask_b32`, carries in) are not followed; such
    // a program still stops with "a constant lane mask" or "an opaque lane mask".
    let (mut seen, mut joins) = (HashSet::new(), HashSet::new());
    while let Some(g) = work.pop() {
        if !seen.insert(g) {
            continue;
        }
        for a in st.bools.support(g) {
            match a {
                Atom::Opaque(_) | Atom::Constant(_) => return true,
                Atom::Phi(id) if joins.insert(id) => {
                    if let Some(edges) = st.phi_masks.get(&id) {
                        work.extend(edges.iter().copied());
                    }
                }
                _ => {}
            }
        }
    }
    false
}

/// Whether accumulator initialisations are also stored at the start of the program
/// ([`lds_init_hoists`]). In this tree off unless `VICTORY_LDS_INIT_HOIST=1` (on by default in
/// fxceless/victory-fork): it applies to every compute program with such slots, so the
/// launcher decides after a game test. Part of the translation (the shader cache's switch list
/// keys on it).
pub(super) fn lds_init_hoist_on() -> bool {
    std::env::var_os("VICTORY_LDS_INIT_HOIST").is_some_and(|v| v == "1")
}

/// The value an LDS dword must start with for the atomic `name` to accumulate into it (`min`
/// of everything with u32::MAX, `max` with 0, ...): `None` for an operation that is not such
/// an accumulation.
fn accumulator_identity(name: &str) -> Option<u32> {
    Some(match name.replace("_rtn", "").as_str() {
        "ds_min_u32" | "ds_and_b32" => u32::MAX,
        "ds_max_u32" | "ds_add_u32" | "ds_or_b32" | "ds_xor_b32" => 0,
        "ds_min_i32" => 0x7FFF_FFFF,
        "ds_max_i32" => 0x8000_0000,
        _ => return None,
    })
}

/// The LDS dwords (below `dwords`, the program's own) that the program initialises for
/// accumulation, with their starting value: a dword every write to which is a `ds_write_b32`
/// or `ds_write2_b32` of one constant at a constant address, and every atomic on which
/// accumulates with that constant as identity (at least one). The title's tile programs have
/// one thread of the group store the starting minimum and maximum (`if (all(tid == 0))`)
/// after the per-pixel early returns, and every thread then `ds_min_u32`/`ds_max_u32`s its
/// key in, with no barrier between (one console wave per group). When that thread has
/// returned early, the atomics combine with whatever LDS held before (on the console another
/// group's values, on the host anything) and the tile comes out wrong: the water's
/// flickering dark patches (5dc3b25986622949 and 6f225b641c5b0708, stores at 0x230 and 0x244).
///
/// Stored by every thread at the program's start as well (then a group barrier), before any
/// LDS access, the dwords hold their starting value whether or not that thread gets there.
/// That is exact: where the program's own store runs it stores the same value again before the
/// dword's first use in that order, and a read that would see the early store instead of a
/// later write of this group would have read memory this group never wrote (undefined).
/// Empty when any LDS write or atomic of the program has an address or (for a write) data that
/// is not a constant of its own block: the dwords it could touch are not known.
pub(in super::super) fn lds_init_hoists(p: &Program, dwords: u32) -> Vec<(u32, u32)> {
    // `x` at instruction `i` of `insts`: a constant set by a plain `v_mov_b32` before it in the
    // block, with EXEC unchanged since (so in every lane that runs `i`)
    let constant = |insts: &[Inst], i: usize, x: Value| -> Option<u32> {
        let d = insts[..i]
            .iter()
            .rposition(|j| j.writes.iter().any(|w| w.1 == x))?;
        let Fields::Valu(f) = &insts[d].fields else {
            return None;
        };
        let plain = insts[d].name() == "v_mov_b32"
            && f.abs == 0
            && f.neg == 0
            && !f.clamp
            && f.omod == 0
            && f.opsel == 0
            && f.sdwa.is_none()
            && f.dpp.is_none()
            && insts[d + 1..i]
                .iter()
                .all(|j| j.write(Reg::S(EXEC)).is_none() && j.write(Reg::S(EXEC + 1)).is_none());
        if !plain {
            return None;
        }
        match f.src.first()? {
            Operand::Const(c) => crate::resource::inline(*c),
            Operand::Literal(k) => Some(*k),
            _ => None,
        }
    };
    let operand = |insts: &[Inst], i: usize, o: Option<&Operand>| -> Option<u32> {
        let &Operand::Vector { reg, .. } = o? else {
            return None;
        };
        constant(insts, i, insts[i].read(Reg::V(reg))?)
    };
    // per dword: the values stored, the identities of the atomics
    let mut stores: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut atomics: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for block in &p.blocks {
        let insts = &block.insts;
        for (i, inst) in insts.iter().enumerate() {
            let Fields::Ds {
                addr,
                data,
                offset0,
                offset1,
                gds,
                ..
            } = &inst.fields
            else {
                continue;
            };
            let name = inst.name();
            if *gds || !super::accesses_lds(inst) || name.starts_with("ds_read") {
                continue;
            }
            let Some(base) = operand(insts, i, addr.as_ref()) else {
                return Vec::new();
            };
            let single = u32::from(*offset0) | u32::from(*offset1) << 8;
            let dword = |bytes: u32| base.wrapping_add(bytes) / 4;
            match name {
                "ds_write_b32" | "ds_write2_b32" => {
                    let at: &[u32] = if name == "ds_write_b32" {
                        &[single]
                    } else {
                        &[u32::from(*offset0) * 4, u32::from(*offset1) * 4]
                    };
                    for (n, &bytes) in at.iter().enumerate() {
                        let Some(k) = operand(insts, i, data.get(n)) else {
                            return Vec::new();
                        };
                        stores.entry(dword(bytes)).or_default().push(k);
                    }
                }
                _ => {
                    let Some(identity) = accumulator_identity(name) else {
                        return Vec::new();
                    };
                    atomics.entry(dword(single)).or_default().push(identity);
                }
            }
        }
    }
    stores
        .into_iter()
        .filter_map(|(dword, values)| {
            let k = values[0];
            let ids = atomics.get(&dword)?;
            (dword < dwords && values.iter().all(|&v| v == k) && ids.iter().all(|&v| v == k))
                .then_some((dword, k))
        })
        .collect()
}
