//! oxtty: finds text the ground state prints unchanged, 16 bytes at a time with NEON on aarch64: valid UTF-8
//! (the rules of `str::from_utf8`, checked as simdjson's and simdutf's lookup algorithm by John Keiser and Daniel
//! Lemire does) holding no C0 controls (ESC among them) and no C1 controls (U+0080..U+009F).

use core::arch::aarch64::*;

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
    // SAFETY: NEON is part of every aarch64 target; each load reads 16 bytes that `i + 16 <= bytes.len()` keeps
    // inside `bytes`.
    let stop = unsafe {
        let tables = (vld1q_u8(BYTE_1_HIGH.as_ptr()), vld1q_u8(BYTE_1_LOW.as_ptr()), vld1q_u8(BYTE_2_HIGH.as_ptr()));
        // What comes before `start` reads as ASCII.
        let mut prev = vdupq_n_u8(0);
        loop {
            if bytes.len() - i < 16 {
                break i;
            }
            let input = vld1q_u8(bytes.as_ptr().add(i));
            let rejected = rejected(input, prev, tables);
            if rejected != 0 {
                break i + rejected.trailing_zeros() as usize / 4;
            }
            prev = input;
            i += 16;
        }
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

/// Four bits for each byte of `input` (the lowest first), set where it is a control or breaks UTF-8 after `prev`.
#[inline(always)]
unsafe fn rejected(input: uint8x16_t, prev: uint8x16_t, tables: (uint8x16_t, uint8x16_t, uint8x16_t)) -> u64 {
    unsafe {
        let prev1 = vextq_u8::<15>(prev, input);
        let prev2 = vextq_u8::<14>(prev, input);
        let prev3 = vextq_u8::<13>(prev, input);
        let byte_1_high = vqtbl1q_u8(tables.0, vshrq_n_u8::<4>(prev1));
        let byte_1_low = vqtbl1q_u8(tables.1, vandq_u8(prev1, vdupq_n_u8(0x0F)));
        let byte_2_high = vqtbl1q_u8(tables.2, vshrq_n_u8::<4>(input));
        let special = vandq_u8(vandq_u8(byte_1_high, byte_1_low), byte_2_high);
        // A third or fourth byte must be a continuation byte; only bytes from 0xE0 and 0xF0 stay at 0x80 and above.
        let third = vqsubq_u8(prev2, vdupq_n_u8(0xE0 - 0x80));
        let fourth = vqsubq_u8(prev3, vdupq_n_u8(0xF0 - 0x80));
        let must_continue = vandq_u8(vorrq_u8(third, fourth), vdupq_n_u8(0x80));
        let invalid = veorq_u8(must_continue, special);
        let c0 = vcltq_u8(input, vdupq_n_u8(0x20));
        let c1 = vandq_u8(vceqq_u8(prev1, vdupq_n_u8(0xC2)), vcltq_u8(input, vdupq_n_u8(0xA0)));
        let rejected = vtstq_u8(vorrq_u8(invalid, vorrq_u8(c0, c1)), vdupq_n_u8(0xFF));
        vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(rejected))))
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
