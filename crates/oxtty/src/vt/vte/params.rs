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
    pub(crate) fn parse(&mut self, bytes: &[u8], mut i: usize) -> Option<usize> {
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

/// oxtty: the CSI parameter of the digits at `bytes[i..]` and how many there are. As the state machine adds one
/// digit at a time saturating at `u16::MAX`, it is the number up to that. Up to seven digits are read from a word
/// at once, without a branch for each.
#[inline(always)]
pub(super) fn csi_param(bytes: &[u8], i: usize) -> (u16, usize) {
    const ZEROS: u64 = 0x3030_3030_3030_3030;
    if let Some(word) = bytes.get(i..i + 8) {
        let word = u64::from_le_bytes(word.try_into().unwrap());
        // The high bit of a byte below '0' or above '9'. Carries and borrows only reach the bytes after it.
        let other = (word.wrapping_add(0x4646_4646_4646_4646) | word.wrapping_sub(ZEROS)) & 0x8080_8080_8080_8080;
        let len = other.trailing_zeros() as usize / 8;
        if len == 0 {
            return (0, 0);
        }
        if len < 8 {
            // The digits as values, the first one the most significant, moved up so the bytes after them drop out
            // and zeros lead; then pairs, quads and all eight are joined.
            let digits = word.wrapping_sub(ZEROS) << (8 * (8 - len));
            let pairs = (digits * 10 + (digits >> 8)) & 0x00FF_00FF_00FF_00FF;
            let quads = (pairs * 100 + (pairs >> 16)) & 0x0000_FFFF_0000_FFFF;
            let value = (quads * 10000 + (quads >> 32)) & 0xFFFF_FFFF;
            return (value.min(u16::MAX as u64) as u16, len);
        }
    }
    let (mut param, mut len) = (0u16, 0);
    while let Some(&byte @ b'0'..=b'9') = bytes.get(i + len) {
        param = param.saturating_mul(10).saturating_add((byte - b'0') as u16);
        len += 1;
    }
    (param, len)
}
