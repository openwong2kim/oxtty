//! oxtty: finds text the ground state prints unchanged, 16 bytes at a time with NEON on aarch64 or SIMD128 on
//! WebAssembly built with it: valid UTF-8 (the rules of `str::from_utf8`, checked as simdjson's and simdutf's
//! lookup algorithm by John Keiser and Daniel Lemire does) holding no C0 controls (ESC among them) and no C1
//! controls (U+0080..U+009F).

/// Errors of the byte before a position and the high nibble of the byte at it (Keiser and Lemire's names).
const TOO_SHORT: u8 = 1 << 0;
const TOO_LONG: u8 = 1 << 1;
const OVERLONG_3: u8 = 1 << 2;
const TOO_LARGE: u8 = 1 << 3;
const SURROGATE: u8 = 1 << 4;
const OVERLONG_2: u8 = 1 << 5;
const TOO_LARGE_1000: u8 = 1 << 6;
const OVERLONG_4: u8 = 1 << 6;
const TWO_CONTS: u8 = 1 << 7;
const CARRY: u8 = TOO_SHORT | TOO_LONG | TWO_CONTS;

/// By the high nibble of the previous byte.
const BYTE_1_HIGH: [u8; 16] = [
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TOO_LONG,
    TWO_CONTS,
    TWO_CONTS,
    TWO_CONTS,
    TWO_CONTS,
    TOO_SHORT | OVERLONG_2,
    TOO_SHORT,
    TOO_SHORT | OVERLONG_3 | SURROGATE,
    TOO_SHORT | TOO_LARGE | TOO_LARGE_1000 | OVERLONG_4,
];

/// By the low nibble of the previous byte.
const BYTE_1_LOW: [u8; 16] = [
    CARRY | OVERLONG_3 | OVERLONG_2 | OVERLONG_4,
    CARRY | OVERLONG_2,
    CARRY,
    CARRY,
    CARRY | TOO_LARGE,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000 | SURROGATE,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
    CARRY | TOO_LARGE | TOO_LARGE_1000,
];

/// By the high nibble of the byte itself.
const BYTE_2_HIGH: [u8; 16] = [
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_LONG | OVERLONG_2 | TWO_CONTS | OVERLONG_3 | TOO_LARGE_1000 | OVERLONG_4,
    TOO_LONG | OVERLONG_2 | TWO_CONTS | OVERLONG_3 | TOO_LARGE,
    TOO_LONG | OVERLONG_2 | TWO_CONTS | SURROGATE | TOO_LARGE,
    TOO_LONG | OVERLONG_2 | TWO_CONTS | SURROGATE | TOO_LARGE,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
    TOO_SHORT,
];

/// Scans `bytes` from `start`, which begins a character, in whole blocks of 16. Returns `(clean, stop)`:
/// `bytes[start..clean]` is valid UTF-8 without controls and ends where a character begins, and `stop >= clean` is
/// the first byte the scan rejected, or the end of the blocks. The characters from `clean` on are for the scalar
/// path, which checks them again.
#[inline(never)]
pub(super) fn printable(bytes: &[u8], start: usize) -> (usize, usize) {
    let mut i = start;
    let mut blocks = Blocks::new();
    let stop = loop {
        if bytes.len() - i < 16 {
            break i;
        }
        let block: &[u8; 16] = bytes[i..i + 16].try_into().unwrap();
        if let Some(at) = blocks.first_rejected(block) {
            break i + at;
        }
        i += 16;
    };
    // No rejection before `stop`, so the characters that end before the one holding `stop - 1` are all valid: a
    // missing continuation byte is rejected where it should be.
    let mut clean = stop;
    if clean > start {
        clean -= 1;
        while clean > start && bytes[clean] & 0xC0 == 0x80 {
            clean -= 1;
        }
    }
    (clean, stop)
}

/// The lookup tables and the block before, for checking one block of 16 after another.
#[cfg(target_arch = "aarch64")]
struct Blocks {
    tables: [core::arch::aarch64::uint8x16_t; 3],
    prev: core::arch::aarch64::uint8x16_t,
}

#[cfg(target_arch = "aarch64")]
impl Blocks {
    #[inline(always)]
    fn new() -> Self {
        use core::arch::aarch64::*;
        // SAFETY: NEON is part of every aarch64 target, and each table is 16 bytes.
        unsafe {
            let tables = [vld1q_u8(BYTE_1_HIGH.as_ptr()), vld1q_u8(BYTE_1_LOW.as_ptr()), vld1q_u8(BYTE_2_HIGH.as_ptr())];
            // What comes before the first block reads as ASCII.
            Self { tables, prev: vdupq_n_u8(0) }
        }
    }

    /// The first byte of `block` that is a control or breaks UTF-8 after the blocks before.
    #[inline(always)]
    fn first_rejected(&mut self, block: &[u8; 16]) -> Option<usize> {
        use core::arch::aarch64::*;
        // SAFETY: NEON is part of every aarch64 target, and `block` is 16 bytes.
        unsafe {
            let (input, prev, tables) = (vld1q_u8(block.as_ptr()), self.prev, self.tables);
            let prev1 = vextq_u8::<15>(prev, input);
            let prev2 = vextq_u8::<14>(prev, input);
            let prev3 = vextq_u8::<13>(prev, input);
            let byte_1_high = vqtbl1q_u8(tables[0], vshrq_n_u8::<4>(prev1));
            let byte_1_low = vqtbl1q_u8(tables[1], vandq_u8(prev1, vdupq_n_u8(0x0F)));
            let byte_2_high = vqtbl1q_u8(tables[2], vshrq_n_u8::<4>(input));
            let special = vandq_u8(vandq_u8(byte_1_high, byte_1_low), byte_2_high);
            // A third or fourth byte must be a continuation byte; only bytes from 0xE0 and 0xF0 stay at 0x80 and
            // above.
            let third = vqsubq_u8(prev2, vdupq_n_u8(0xE0 - 0x80));
            let fourth = vqsubq_u8(prev3, vdupq_n_u8(0xF0 - 0x80));
            let must_continue = vandq_u8(vorrq_u8(third, fourth), vdupq_n_u8(0x80));
            let invalid = veorq_u8(must_continue, special);
            let c0 = vcltq_u8(input, vdupq_n_u8(0x20));
            let c1 = vandq_u8(vceqq_u8(prev1, vdupq_n_u8(0xC2)), vcltq_u8(input, vdupq_n_u8(0xA0)));
            let rejected = vtstq_u8(vorrq_u8(invalid, vorrq_u8(c0, c1)), vdupq_n_u8(0xFF));
            // Four bits for each byte, the lowest first.
            let bits = vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(rejected))));
            self.prev = input;
            (bits != 0).then(|| bits.trailing_zeros() as usize / 4)
        }
    }
}

/// The lookup tables and the block before, for checking one block of 16 after another.
#[cfg(target_arch = "wasm32")]
struct Blocks {
    tables: [core::arch::wasm32::v128; 3],
    prev: core::arch::wasm32::v128,
}

#[cfg(target_arch = "wasm32")]
impl Blocks {
    #[inline(always)]
    fn new() -> Self {
        use core::arch::wasm32::*;
        let table = |t: &[u8; 16]| u8x16(t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7], t[8], t[9], t[10], t[11],
            t[12], t[13], t[14], t[15]);
        // What comes before the first block reads as ASCII.
        Self { tables: [table(&BYTE_1_HIGH), table(&BYTE_1_LOW), table(&BYTE_2_HIGH)], prev: u8x16_splat(0) }
    }

    /// The first byte of `block` that is a control or breaks UTF-8 after the blocks before (as for aarch64).
    #[inline(always)]
    fn first_rejected(&mut self, block: &[u8; 16]) -> Option<usize> {
        use core::arch::wasm32::*;
        // SAFETY: `block` is 16 bytes; `v128_load` needs no alignment.
        let input = unsafe { v128_load(block.as_ptr().cast()) };
        let (prev, tables) = (self.prev, self.tables);
        let prev1 = i8x16_shuffle::<15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30>(prev, input);
        let prev2 = i8x16_shuffle::<14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29>(prev, input);
        let prev3 = i8x16_shuffle::<13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28>(prev, input);
        let byte_1_high = u8x16_swizzle(tables[0], u8x16_shr(prev1, 4));
        let byte_1_low = u8x16_swizzle(tables[1], v128_and(prev1, u8x16_splat(0x0F)));
        let byte_2_high = u8x16_swizzle(tables[2], u8x16_shr(input, 4));
        let special = v128_and(v128_and(byte_1_high, byte_1_low), byte_2_high);
        let third = u8x16_sub_sat(prev2, u8x16_splat(0xE0 - 0x80));
        let fourth = u8x16_sub_sat(prev3, u8x16_splat(0xF0 - 0x80));
        let must_continue = v128_and(v128_or(third, fourth), u8x16_splat(0x80));
        let invalid = v128_xor(must_continue, special);
        let c0 = u8x16_lt(input, u8x16_splat(0x20));
        let c1 = v128_and(u8x16_eq(prev1, u8x16_splat(0xC2)), u8x16_lt(input, u8x16_splat(0xA0)));
        let rejected = v128_or(u8x16_ne(invalid, u8x16_splat(0)), v128_or(c0, c1));
        // One bit for each byte, the lowest first.
        let bits = u8x16_bitmask(rejected);
        self.prev = input;
        (bits != 0).then(|| bits.trailing_zeros() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printable_text(text: &str) -> bool {
        !text.chars().any(|c| c < ' ' || ('\u{80}'..='\u{9f}').contains(&c))
    }

    /// The scan claims only printable text that ends on a character, and rejects no block of printable text.
    fn check(bytes: &[u8]) {
        let (clean, stop) = printable(bytes, 0);
        assert!(clean <= stop && stop <= bytes.len(), "{bytes:02x?} {clean} {stop}");
        let text = core::str::from_utf8(&bytes[..clean]).unwrap_or_else(|e| panic!("{bytes:02x?} {e}"));
        assert!(printable_text(text), "{bytes:02x?}");
        let whole = bytes.len() / 16 * 16;
        if core::str::from_utf8(&bytes[..whole]).is_ok_and(printable_text) {
            assert_eq!(stop, whole, "{bytes:02x?}");
        }
    }

    #[test]
    fn printable_matches_from_utf8() {
        // Text with one or two bytes replaced at every position, so each error and control lands at every place
        // in a block and across the boundary between two.
        let text = "ab가😀é漢\u{a0}字\u{7ff}\u{800}\u{ffff}\u{10000}\u{10ffff}z ~\u{7f}".repeat(3);
        let bytes = text.as_bytes();
        check(bytes);
        let pick = [0x00, 0x1b, 0x1f, 0x20, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xc1, 0xc2, 0xdf, 0xe0,
            0xed, 0xef, 0xf0, 0xf4, 0xf5, 0xff];
        for at in 0..bytes.len() {
            for &b in &pick {
                let mut changed = bytes.to_vec();
                changed[at] = b;
                check(&changed);
                if at + 1 < bytes.len() {
                    for &c in &pick {
                        changed[at + 1] = c;
                        check(&changed);
                    }
                }
            }
        }
        // Random bytes, mostly high ones.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..50_000 {
            let mut bytes = [0u8; 48];
            for b in &mut bytes {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *b = seed as u8 | if seed >> 40 & 3 == 0 { 0 } else { 0x80 };
            }
            check(&bytes);
        }
    }
}
