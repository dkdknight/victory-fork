//! The texture digest with AVX2 (`VICTORY_TEXTURE_HASH_FAST=1`, off): 128
//! bytes a step in sixteen 64-bit lanes of `vpmuludq` products, after
//! fxceless PR #1395 (n-romanovich). Ours ([`super::digest`]) already left
//! BLAKE3 for 128-bit multiplications folded, about 27-30 GB/s on one
//! thread over a texture in the cache; this one takes about 50-58, and a
//! fifth more than ours over one that is not (memory bound, 10-13 GB/s).
//!
//! It only has to tell that a texture's memory changed since its last
//! look, and nobody makes up textures to collide. A change of the bytes one
//! lane takes in leaves that lane's 64 bits different but for a chance of
//! about 2^-64 a 128-byte step after it (a step's product of two 32-bit
//! halves meets another one's only at one difference in a lattice of 2^64),
//! so at most 2^-64 times the steps (2^-41 for a texture of a gigabyte,
//! 2^-49 for one of four megabytes). The lanes are then folded through
//! `mix64` one after the other, a bijection of each lane: one lane's
//! difference always changes the digest, two lanes' cancel at about 2^-64
//! in each half. A change missed would leave the staging buffer's old
//! texels until the texture's next change.
//!
//! Unlike the PR, every lane starts from a seed of its own and the lanes
//! are not folded by XOR: lanes that started equal took zeros alike, and
//! their XOR told a byte changed at 3 from the same change at 83 (576 of
//! the 2000 inputs of [`tests::tells_bytes_apart`] met another's digest).
//! The step itself is the PR's.
//!
//! The scalar path takes the same lanes in the same order, so whether a
//! processor has AVX2 never changes a digest (tested below); a run takes
//! one kind of digest throughout ([`on`] is read once), so a digest of one
//! kind is never held against one of the other.

/// `VICTORY_TEXTURE_HASH_FAST=1`: texture digests are this hash. Read once
/// for the run.
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_TEXTURE_HASH_FAST").is_some_and(|v| v == "1"))
}

/// The seeds of the tail and the fold (the length is mixed in at the end).
const K: [u64; 4] = [
    0x9e37_79b9_7f4a_7c15,
    0xc2b2_ae3d_27d4_eb4f,
    0x1656_67b1_9e37_79f9,
    0x85eb_ca77_c2b2_ae63,
];
/// A seed for each of the sixteen lanes (lane `i` takes bytes `8 i..8 i +
/// 8` of every 128).
const SEEDS: [u64; 16] = {
    let mut s = [0; 16];
    let mut i = 0;
    while i < 16 {
        s[i] = mix64(K[0].wrapping_mul(i as u64 + 1) ^ K[3]);
        i += 1;
    }
    s
};

/// The 32-bit factors of a lane's low and high half (swapped in odd
/// registers).
const M: [u64; 2] = [0x9e37_79b1, 0x85eb_ca77];

/// The 128-bit hash of `bytes`.
pub(crate) fn hash(bytes: &[u8]) -> [u64; 2] {
    #[cfg(target_arch = "x86_64")]
    {
        static AVX2: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *AVX2.get_or_init(|| std::is_x86_feature_detected!("avx2")) {
            // SAFETY: the processor has AVX2.
            return unsafe { hash_avx2(bytes) };
        }
    }
    hash_scalar(bytes)
}

#[inline]
const fn mix64(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// The lanes folded one after the other (each half a chain of `mix64`,
/// the other way round for the high one), with the hash of what is left
/// after the last whole 128 bytes, and the length.
#[inline]
fn finish(lanes: [u64; 16], rest: &[u8], len: usize) -> [u64; 2] {
    let tail = if rest.is_empty() { [0, 0] } else { tail(rest) };
    let len = len as u64;
    let mut lo = K[1] ^ len;
    let mut hi = K[2] ^ len.rotate_left(32);
    for i in 0..16 {
        lo = mix64(lo ^ lanes[i]);
        hi = mix64(hi.wrapping_add(lanes[15 - i]));
    }
    // (Both halves as they are: the low one alone is a bijection of each
    // lane and of the tail's low word.)
    let lo = mix64(lo ^ tail[0]);
    let hi = mix64(hi ^ tail[1] ^ lo.rotate_left(19));
    [lo, hi]
}

/// Bytes short of 128: four 64-bit lanes of full multiplications.
fn tail(bytes: &[u8]) -> [u64; 2] {
    let mut s0 = K[0] ^ (bytes.len() as u64);
    let mut s1 = K[1];
    let mut s2 = K[2];
    let mut s3 = K[3];
    let word = |b: &[u8]| {
        let mut w = [0; 8];
        w[..b.len()].copy_from_slice(b);
        u64::from_le_bytes(w)
    };
    let mut chunks = bytes.chunks_exact(32);
    for c in &mut chunks {
        s0 = (s0 ^ word(&c[0..8])).wrapping_mul(K[1]).rotate_left(27);
        s1 = (s1 ^ word(&c[8..16])).wrapping_mul(K[2]).rotate_left(31);
        s2 = (s2 ^ word(&c[16..24])).wrapping_mul(K[3]).rotate_left(29);
        s3 = (s3 ^ word(&c[24..32])).wrapping_mul(K[0]).rotate_left(33);
    }
    let mut rem = chunks.remainder();
    while rem.len() >= 8 {
        s0 = (s0 ^ word(&rem[..8])).wrapping_mul(K[2]).rotate_left(29);
        rem = &rem[8..];
    }
    if !rem.is_empty() {
        s1 = (s1 ^ word(rem)).wrapping_mul(K[3]).rotate_left(31);
    }
    let a = mix64(s0 ^ s2 ^ (bytes.len() as u64));
    let b = mix64(s1 ^ s3 ^ a.rotate_left(17));
    [a ^ b.rotate_left(32), mix64(b ^ a)]
}

/// [`hash_avx2`] a lane at a time: lane `i` takes bytes `8 i..8 i + 8` of
/// each step (register `i / 4` of the four, its lane `i % 4`).
fn hash_scalar(bytes: &[u8]) -> [u64; 2] {
    let mut lanes = SEEDS;
    let mut steps = bytes.chunks_exact(128);
    for step in &mut steps {
        for (i, lane) in lanes.iter_mut().enumerate() {
            // Odd registers take the factors the other way round.
            let (lo, hi) = if (i / 4) % 2 == 0 {
                (M[0], M[1])
            } else {
                (M[1], M[0])
            };
            let x = *lane ^ u64::from_le_bytes(step[8 * i..8 * i + 8].try_into().unwrap_or([0; 8]));
            *lane = (x & 0xffff_ffff)
                .wrapping_mul(lo)
                .wrapping_add((x >> 32).wrapping_mul(hi));
        }
    }
    finish(lanes, steps.remainder(), bytes.len())
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn hash_avx2(bytes: &[u8]) -> [u64; 2] {
    use std::arch::x86_64::*;
    // SAFETY: `SEEDS` holds four 256-bit loads.
    let [mut a0, mut a1, mut a2, mut a3] =
        unsafe { [0, 4, 8, 12].map(|i| _mm256_loadu_si256(SEEDS.as_ptr().add(i).cast())) };
    let mul0 = _mm256_set1_epi32(M[0] as u32 as i32);
    let mul1 = _mm256_set1_epi32(M[1] as u32 as i32);
    let mut steps = bytes.chunks_exact(128);
    for step in &mut steps {
        let p = step.as_ptr();
        // SAFETY: 128 bytes at `p`; unaligned loads.
        let [v0, v1, v2, v3] =
            unsafe { [0, 32, 64, 96].map(|at| _mm256_loadu_si256(p.add(at).cast())) };
        let x0 = _mm256_xor_si256(a0, v0);
        let x1 = _mm256_xor_si256(a1, v1);
        let x2 = _mm256_xor_si256(a2, v2);
        let x3 = _mm256_xor_si256(a3, v3);
        // Low half times one factor, plus high half times the other.
        a0 = _mm256_add_epi64(
            _mm256_mul_epu32(x0, mul0),
            _mm256_mul_epu32(_mm256_srli_epi64(x0, 32), mul1),
        );
        a1 = _mm256_add_epi64(
            _mm256_mul_epu32(x1, mul1),
            _mm256_mul_epu32(_mm256_srli_epi64(x1, 32), mul0),
        );
        a2 = _mm256_add_epi64(
            _mm256_mul_epu32(x2, mul0),
            _mm256_mul_epu32(_mm256_srli_epi64(x2, 32), mul1),
        );
        a3 = _mm256_add_epi64(
            _mm256_mul_epu32(x3, mul1),
            _mm256_mul_epu32(_mm256_srli_epi64(x3, 32), mul0),
        );
    }
    let mut lanes = [0u64; 16];
    // SAFETY: `lanes` holds four 256-bit stores.
    unsafe {
        for (i, a) in [a0, a1, a2, a3].into_iter().enumerate() {
            _mm256_storeu_si256(lanes.as_mut_ptr().add(4 * i).cast(), a);
        }
    }
    finish(lanes, steps.remainder(), bytes.len())
}

#[cfg(test)]
mod tests {
    use super::{hash, hash_scalar};

    /// splitmix64: random bytes without a crate.
    fn bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
                super::mix64(s) as u8
            })
            .collect()
    }

    /// The AVX2 path and the scalar one agree, over every length to 1100
    /// and others up to a megabyte, from every alignment to 64.
    #[test]
    fn avx2_and_scalar_agree() {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            let all = bytes(1, (1 << 20) + 4096);
            let mut lens: Vec<usize> = (0..=1100).collect();
            lens.extend([
                4095,
                4096,
                4097,
                65_535,
                65_536,
                65_537 + 13,
                1 << 20,
                (1 << 20) + 127,
            ]);
            let mut n = 0u64;
            for &len in &lens {
                for at in 0..64usize {
                    if (at % 7 != 0 && at != 63) && len > 1100 {
                        continue;
                    }
                    let b = &all[at..at + len];
                    // SAFETY: the processor has AVX2.
                    let fast = unsafe { super::hash_avx2(b) };
                    assert_eq!(fast, hash_scalar(b), "len {len}, at {at}");
                    n += 1;
                }
            }
            // And random contents per length.
            for seed in 0..200u64 {
                let len = (super::mix64(seed) % 5000) as usize;
                let b = bytes(seed + 100, len + 64);
                let at = (seed % 64) as usize;
                // SAFETY: the processor has AVX2.
                assert_eq!(
                    unsafe { super::hash_avx2(&b[at..at + len]) },
                    hash_scalar(&b[at..at + len]),
                    "seed {seed}"
                );
                n += 1;
            }
            assert!(n > 70_000);
            return;
        }
        eprintln!("no AVX2: the paths were not compared");
    }

    /// Every length and every changed byte tell apart, zeros among them (as
    /// for [`super::super::digest`]), and single bits anywhere.
    #[test]
    fn tells_bytes_apart() {
        let zeros = [0u8; 400];
        let mut seen: Vec<[u64; 2]> = (0..400).map(|n| hash(&zeros[..n])).collect();
        for at in 0..400 {
            for value in [1u8, 0x80, 0xff] {
                let mut b = zeros;
                b[at] = value;
                seen.push(hash(&b));
            }
        }
        let text: Vec<u8> = (0..400u32).map(|i| (i * 37 >> 2) as u8).collect();
        for at in 0..399 {
            let mut b = text.clone();
            b.swap(at, at + 1);
            if b != text {
                seen.push(hash(&b));
            }
        }
        seen.push(hash(&text));
        let n = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), n);
        for len in [1usize, 127, 128, 129, 255, 256, 513, 4096, 1 << 16] {
            let mut b = bytes(len as u64, len);
            let base = hash(&b);
            for at in [0, len / 2, len - 1] {
                for bit in [0, 31, 32, 63] {
                    let (byte, mask) = ((at & !7) + bit / 8, 1u8 << (bit % 8));
                    if byte >= len {
                        continue;
                    }
                    b[byte] ^= mask;
                    assert_ne!(hash(&b), base, "len {len}, byte {byte}, bit {bit}");
                    b[byte] ^= mask;
                }
            }
            assert_eq!(hash(&b), base);
        }
        // Two 128-byte steps swapped.
        let b = bytes(7, 1024);
        let mut swapped = b.clone();
        swapped[..256].rotate_left(128);
        assert_ne!(hash(&b), hash(&swapped));
    }
}
