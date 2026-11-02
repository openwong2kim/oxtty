//! Fixed size parameters list with optional subparameters.

use core::fmt::{self, Debug, Formatter};

pub(crate) const MAX_PARAMS: usize = 32;

#[derive(Default)]
pub struct Params {
    /// Number of subparameters for each parameter.
    ///
    /// For each entry in the `params` slice, this stores the length of the
    /// param as number of subparams at the same index as the param in the
    /// `params` slice.
    ///
    /// At the subparam positions the length will always be `0`.
    subparams: [u8; MAX_PARAMS],

    /// All parameters and subparameters.
    params: [u16; MAX_PARAMS],

    /// Number of suparameters in the current parameter.
    current_subparams: u8,

    /// Total number of parameters and subparameters.
    len: usize,

    /// Which entries of `params` were empty subparameters, one bit each (oxtty).
    empty: u32,
}

impl Params {
    /// Returns the number of parameters.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if there are no parameters present.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns an iterator over all parameters and subparameters.
    #[inline]
    pub fn iter(&self) -> ParamsIter<'_> {
        ParamsIter::new(self)
    }

    /// Returns `true` if there is no more space for additional parameters.
    #[inline]
    pub(crate) fn is_full(&self) -> bool {
        self.len == MAX_PARAMS
    }

    /// Whether the entry at `index` of all parameters and subparameters, in [`Params::iter`] order, was an empty
    /// subparameter (oxtty). It reads as 0, as an empty parameter does; xterm.js reports it as -1.
    #[inline]
    pub fn is_empty_subparam(&self, index: usize) -> bool {
        index < self.len && self.empty >> index & 1 != 0
    }

    /// Clear all parameters.
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.current_subparams = 0;
        self.len = 0;
        self.empty = 0;
    }

    /// Marks the entry about to be added as empty if it is a subparameter (oxtty).
    #[inline]
    pub(crate) fn mark_empty(&mut self) {
        if self.current_subparams > 0 {
            self.empty |= 1 << self.len;
        }
    }

    /// Parses the parameters of a CSI at `bytes[i..]` into the cleared list, as the parser would one byte at a
    /// time, and returns where the first byte that is not a digit, `:` or `;` is (oxtty). `None` where the bytes end
    /// first or the parameters would fill the list, whose handling the parser keeps.
    #[inline(always)]
    pub(crate) fn parse(&mut self, bytes: &[u8], i: usize) -> Option<usize> {
        match self.parse_words(bytes, i) {
            Some(end) => end,
            None => self.parse_bytes(bytes, i),
        }
    }

    /// `parse` with the separators and the end of the parameters found eight bytes at a time first, so each
    /// parameter's place is known before its digits are read. `None` where the parameters end within the first
    /// eight bytes (the byte path is quicker for a short list), or not within 64 bytes that are all there.
    #[inline(always)]
    fn parse_words(&mut self, bytes: &[u8], i: usize) -> Option<Option<usize>> {
        let (mut separators, mut end) = (0u64, None);
        for word_at in (0..64).step_by(8) {
            let word = u64::from_le_bytes(bytes.get(i + word_at..i + word_at + 8)?.try_into().unwrap());
            let (separator, other) = classify(word);
            if other != 0 && word_at == 0 {
                return None;
            }
            separators |= bits(separator) << word_at;
            if other != 0 {
                end = Some(word_at + bits(other).trailing_zeros() as usize);
                break;
            }
        }
        let end = end?;
        separators &= (1 << end) - 1;
        if separators.count_ones() as usize >= MAX_PARAMS - 1 {
            return Some(None);
        }
        let (mut len, mut current, mut empty, mut start) = (0, 0, 0, 0);
        loop {
            let stop = if separators == 0 { end } else { separators.trailing_zeros() as usize };
            if stop == start && current > 0 {
                empty |= 1 << len;
            }
            self.subparams[len - current] = current as u8 + 1;
            self.params[len] = digits_value(bytes, i + start, stop - start);
            len += 1;
            if separators == 0 {
                break;
            }
            current = if bytes[i + stop] == b':' { current + 1 } else { 0 };
            separators &= separators - 1;
            start = stop + 1;
        }
        (self.len, self.current_subparams, self.empty) = (len, 0, empty);
        Some(Some(i + end))
    }

    /// `parse` one parameter at a time.
    #[inline]
    fn parse_bytes(&mut self, bytes: &[u8], mut i: usize) -> Option<usize> {
        let (mut len, mut current, mut empty) = (0, 0, 0);
        loop {
            if len >= MAX_PARAMS - 1 {
                return None;
            }
            let (param, digits) = csi_param(bytes, i);
            i += digits;
            let byte = *bytes.get(i)?;
            if digits == 0 && current > 0 {
                empty |= 1 << len;
            }
            self.subparams[len - current] = current as u8 + 1;
            self.params[len] = param;
            len += 1;
            match byte {
                b':' => current += 1,
                b';' => current = 0,
                _ => break,
            }
            i += 1;
        }
        (self.len, self.current_subparams, self.empty) = (len, 0, empty);
        Some(i)
    }

    /// Add an additional parameter.
    #[inline]
    pub(crate) fn push(&mut self, item: u16) {
        self.subparams[self.len - self.current_subparams as usize] = self.current_subparams + 1;
        self.params[self.len] = item;
        self.current_subparams = 0;
        self.len += 1;
    }

    /// Add an additional subparameter to the current parameter.
    #[inline]
    pub(crate) fn extend(&mut self, item: u16) {
        self.subparams[self.len - self.current_subparams as usize] = self.current_subparams + 1;
        self.params[self.len] = item;
        self.current_subparams += 1;
        self.len += 1;
    }
}

impl<'a> IntoIterator for &'a Params {
    type IntoIter = ParamsIter<'a>;
    type Item = &'a [u16];

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Immutable subparameter iterator.
pub struct ParamsIter<'a> {
    params: &'a Params,
    index: usize,
}

impl<'a> ParamsIter<'a> {
    fn new(params: &'a Params) -> Self {
        Self { params, index: 0 }
    }
}

impl<'a> Iterator for ParamsIter<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.params.len() {
            return None;
        }

        // Get all subparameters for the current parameter.
        let num_subparams = self.params.subparams[self.index];
        let param = &self.params.params[self.index..self.index + num_subparams as usize];

        // Jump to the next parameter.
        self.index += num_subparams as usize;

        Some(param)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.params.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl Debug for Params {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;

        for (i, param) in self.iter().enumerate() {
            if i != 0 {
                write!(f, ";")?;
            }

            for (i, subparam) in param.iter().enumerate() {
                if i != 0 {
                    write!(f, ":")?;
                }

                subparam.fmt(f)?;
            }
        }

        write!(f, "]")
    }
}

const ZEROS: u64 = 0x3030_3030_3030_3030;

/// oxtty: the high bit of each byte of `word` that is `:` or `;`, and of each that is not a digit either. Each
/// byte is tested on its own: no sum carries out of a byte below 0x80.
#[inline(always)]
fn classify(word: u64) -> (u64, u64) {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let low = word & !HIGH;
    let from_zero = low.wrapping_add(0x5050_5050_5050_5050);
    let above_nine = low.wrapping_add(0x4646_4646_4646_4646);
    let above_semicolon = low.wrapping_add(0x4444_4444_4444_4444);
    let parameter = from_zero & !above_semicolon & !word & HIGH;
    (parameter & above_nine, !parameter & HIGH)
}

/// oxtty: the high bits of the bytes of `high` as one bit each, the lowest byte's lowest.
#[inline(always)]
fn bits(high: u64) -> u64 {
    (high >> 7).wrapping_mul(0x0102_0408_1020_4080) >> 56
}

/// oxtty: the CSI parameter of the `len` digits at `bytes[at..]`. As the state machine adds one digit at a time
/// saturating at `u16::MAX`, it is the number up to that.
#[inline(always)]
fn digits_value(bytes: &[u8], at: usize, len: usize) -> u16 {
    if len == 0 {
        return 0;
    }
    match bytes.get(at..at + 8) {
        Some(word) if len <= 8 => word_value(u64::from_le_bytes(word.try_into().unwrap()), len),
        _ => bytes[at..at + len]
            .iter()
            .fold(0, |param: u16, byte| param.saturating_mul(10).saturating_add((byte - b'0') as u16)),
    }
}

/// oxtty: the value of the first `len` (1 to 8) bytes of `word`, which are digits, saturated at `u16::MAX`.
#[inline(always)]
fn word_value(word: u64, len: usize) -> u16 {
    // The digits as values, the first one the most significant, moved up so the bytes after them drop out and
    // zeros lead (borrows only reach the bytes after them); then pairs, quads and all eight are joined. Up to
    // four digits (most parameters) need only the low half.
    if len <= 4 {
        let digits = (word as u32).wrapping_sub(ZEROS as u32) << (8 * (4 - len));
        let pairs = (digits * 10 + (digits >> 8)) & 0x00FF_00FF;
        return ((pairs * 100 + (pairs >> 16)) & 0xFFFF) as u16;
    }
    let digits = word.wrapping_sub(ZEROS) << (8 * (8 - len));
    let pairs = (digits * 10 + (digits >> 8)) & 0x00FF_00FF_00FF_00FF;
    let quads = (pairs * 100 + (pairs >> 16)) & 0x0000_FFFF_0000_FFFF;
    let value = (quads * 10000 + (quads >> 32)) & 0xFFFF_FFFF;
    value.min(u16::MAX as u64) as u16
}

/// oxtty: the CSI parameter of the digits at `bytes[i..]` and how many there are. Up to seven digits are read
/// from a word at once, without a branch for each.
#[inline(always)]
fn csi_param(bytes: &[u8], i: usize) -> (u16, usize) {
    if let Some(word) = bytes.get(i..i + 8) {
        let word = u64::from_le_bytes(word.try_into().unwrap());
        // The high bit of a byte below '0' or above '9'. Carries and borrows only reach the bytes after it.
        let other = (word.wrapping_add(0x4646_4646_4646_4646) | word.wrapping_sub(ZEROS)) & 0x8080_8080_8080_8080;
        let len = other.trailing_zeros() as usize / 8;
        if len < 8 {
            return (if len == 0 { 0 } else { word_value(word, len) }, len);
        }
    }
    let (mut param, mut len) = (0u16, 0);
    while let Some(&byte @ b'0'..=b'9') = bytes.get(i + len) {
        param = param.saturating_mul(10).saturating_add((byte - b'0') as u16);
        len += 1;
    }
    (param, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csi_param_matches_digit_by_digit() {
        let mut seed = 0x853c_49e6_748f_ea9bu64;
        for _ in 0..100_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            // Digits, then anything; zeros often, so long numbers can stay small.
            let len = (seed % 12) as usize;
            let mut bytes: Vec<u8> = (0..len)
                .map(|k| if seed >> (20 + k) & 1 == 0 { b'0' } else { b'0' + (seed >> (4 * k % 60)) as u8 % 10 })
                .collect();
            bytes.extend((0..(seed >> 60) as usize).map(|k| [b';', b':', b'm', b'/', b'0', 0x80, 0xff][k % 7]));
            let at = ((seed >> 33) as usize % 2).min(bytes.len());
            let expected = bytes[at..].iter().take_while(|b| b.is_ascii_digit()).fold((0u16, 0), |(p, n), b| {
                (p.saturating_mul(10).saturating_add((b - b'0') as u16), n + 1)
            });
            assert_eq!(csi_param(&bytes, at), expected, "{bytes:?} {at}");
            assert_eq!(digits_value(&bytes, at, expected.1), expected.0, "{bytes:?} {at}");
        }
    }
}
