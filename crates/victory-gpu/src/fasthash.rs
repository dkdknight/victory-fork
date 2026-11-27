//! A hash for small keys of ours (addresses, lengths, indices), not for
//! anything a stranger chooses: one multiply per word, where the standard
//! library's SipHash took ~4 % of the recording threads in the maps asked on
//! every draw (the lookup memory, the refreshes of a list).

use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub(crate) struct Fast(u64);

impl Hasher for Fast {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }
    fn write_u8(&mut self, v: u8) {
        self.write_u64(u64::from(v));
    }
    fn write_u32(&mut self, v: u32) {
        self.write_u64(u64::from(v));
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type FastMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<Fast>>;
pub(crate) type FastSet<K> = std::collections::HashSet<K, BuildHasherDefault<Fast>>;
