// Copyright (c) 2017 Baidu, Inc. All Rights Reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
//
//  * Redistributions of source code must retain the above copyright
//    notice, this list of conditions and the following disclaimer.
//  * Redistributions in binary form must reproduce the above copyright
//    notice, this list of conditions and the following disclaimer in
//    the documentation and/or other materials provided with the
//    distribution.
//  * Neither the name of Baidu, Inc., nor the names of its
//    contributors may be used to endorse or promote products derived
//    from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
// "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
// LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
// A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
// OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
// LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
// DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
// THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
// (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

//! Implementation of [the WTF-8 encoding]
//!

// this module is imported from @SimonSapin's repo and has tons of dead code on
// unix (it's mostly used on windows), so don't worry about dead code here.
#![allow(dead_code)]

use ascii::*;
use alloc::borrow::Cow;
use std_unicode::char;
use core::str::next_code_point;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::iter::FromIterator;
use core::mem;
use core::ops;
use core::str;
use alloc::slice;
use sys_common::AsInner;

const UTF8_REPLACEMENT_CHARACTER: &'static str = "\u{FFFD}";

/// A Unicode code point: from U+0000 to U+10FFFF.
///
/// Compare with the `char` type,
/// which represents a Unicode scalar value:
/// a code point that is not a surrogate (U+D800 to U+DFFF).
#[derive(Eq, PartialEq, Ord, PartialOrd, Clone, Copy)]
pub struct CodePoint {
    value: u32
}

/// Format the code point as `U+` followed by four to six hexadecimal digits.
/// Example: `U+1F4A9`
impl fmt::Debug for CodePoint {
    #[inline]
    fn fmt(&self, formatter: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(formatter, "U+{:04X}", self.value)
    }
}

impl CodePoint {
    /// Unsafely creates a new `CodePoint` without checking the value.
    ///
    /// Only use when `value` is known to be less than or equal to 0x10FFFF.
    #[inline]
    pub unsafe fn from_u32_unchecked(value: u32) -> CodePoint {
        CodePoint { value: value }
    }

    /// Creates a new `CodePoint` if the value is a valid code point.
    ///
    /// Returns `None` if `value` is above 0x10FFFF.
    #[inline]
    pub fn from_u32(value: u32) -> Option<CodePoint> {
        match value {
            0 ... 0x10FFFF => Some(CodePoint { value: value }),
            _ => None
        }
    }

    /// Creates a new `CodePoint` from a `char`.
    ///
    /// Since all Unicode scalar values are code points, this always succeeds.
    #[inline]
    pub fn from_char(value: char) -> CodePoint {
        CodePoint { value: value as u32 }
    }

    /// Returns the numeric value of the code point.
    #[inline]
    pub fn to_u32(&self) -> u32 {
        self.value
    }

    /// Optionally returns a Unicode scalar value for the code point.
    ///
    /// Returns `None` if the code point is a surrogate (from U+D800 to U+DFFF).
    #[inline]
    pub fn to_char(&self) -> Option<char> {
        match self.value {
            0xD800 ... 0xDFFF => None,
            _ => Some(unsafe { char::from_u32_unchecked(self.value) })
        }
    }

    /// Returns a Unicode scalar value for the code point.
    ///
    /// Returns `'\u{FFFD}'` (the replacement character “�”)
    /// if the code point is a surrogate (from U+D800 to U+DFFF).
    #[inline]
    pub fn to_char_lossy(&self) -> char {
        self.to_char().unwrap_or('\u{FFFD}')
    }
}

/// An owned, growable string of well-formed WTF-8 data.
///
/// Similar to `String`, but can additionally contain surrogate code points
/// if they’re not in a surrogate pair.
#[derive(Eq, PartialEq, Ord, PartialOrd, Clone)]
pub struct Wtf8Buf {
    bytes: Vec<u8>
}

impl ops::Deref for Wtf8Buf {
    type Target = Wtf8;

    fn deref(&self) -> &Wtf8 {
        self.as_slice()
    }
}

/// Format the string with double quotes,
/// and surrogates as `\u` followed by four hexadecimal digits.
/// Example: `"a\u{D800}"` for a string with code points [U+0061, U+D800]
impl fmt::Debug for Wtf8Buf {
    #[inline]
    fn fmt(&self, formatter: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        fmt::Debug::fmt(&**self, formatter)
    }
}

impl Wtf8Buf {
    /// Creates a new, empty WTF-8 string.
    #[inline]
    pub fn new() -> Wtf8Buf {
        Wtf8Buf { bytes: Vec::new() }
    }

    /// Creates a new, empty WTF-8 string with pre-allocated capacity for `n` bytes.
    #[inline]
    pub fn with_capacity(n: usize) -> Wtf8Buf {
        Wtf8Buf { bytes: Vec::with_capacity(n) }
    }

    /// Creates a WTF-8 string from a UTF-8 `String`.
    ///
    /// This takes ownership of the `String` and does not copy.
    ///
    /// Since WTF-8 is a superset of UTF-8, this always succeeds.
    #[inline]
    pub fn from_string(string: String) -> Wtf8Buf {
        Wtf8Buf { bytes: string.into_bytes() }
    }

    /// Creates a WTF-8 string from a UTF-8 `&str` slice.
    ///
    /// This copies the content of the slice.
    ///
    /// Since WTF-8 is a superset of UTF-8, this always succeeds.
    #[inline]
    pub fn from_str(str: &str) -> Wtf8Buf {
        Wtf8Buf { bytes: <[_]>::to_vec(str.as_bytes()) }
    }

    pub fn clear(&mut self) {
        self.bytes.clear()
    }

    /// Creates a WTF-8 string from a potentially ill-formed UTF-16 slice of 16-bit code units.
    ///
    /// This is lossless: calling `.encode_wide()` on the resulting string
    /// will always return the original code units.
    pub fn from_wide(v: &[u16]) -> Wtf8Buf {
        let mut string = Wtf8Buf::with_capacity(v.len());
        for item in char::decode_utf16(v.iter().cloned()) {
            match item {
                Ok(ch) => string.push_char(ch),
                Err(surrogate) => {
                    let surrogate = surrogate.unpaired_surrogate();
                    // Surrogates are known to be in the code point range.
                    let code_point = unsafe {
                        CodePoint::from_u32_unchecked(surrogate as u32)
                    };
                    // Skip the WTF-8 concatenation check,
                    // surrogate pairs are already decoded by decode_utf16
                    string.push_code_point_unchecked(code_point)
                }
            }
        }
        string
    }

    /// Copied from String::push
    /// This does **not** include the WTF-8 concatenation check.
    fn push_code_point_unchecked(&mut self, code_point: CodePoint) {
        let c = unsafe {
            char::from_u32_unchecked(code_point.value)
        };
        let mut bytes = [0; 4];
        let bytes = c.encode_utf8(&mut bytes).as_bytes();
        self.bytes.extend_from_slice(bytes)
    }

    #[inline]
    pub fn as_slice(&self) -> &Wtf8 {
        unsafe { Wtf8::from_bytes_unchecked(&self.bytes) }
    }

    /// Reserves capacity for at least `additional` more bytes to be inserted
    /// in the given `Wtf8Buf`.
    /// The collection may reserve more space to avoid frequent reallocations.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity overflows `usize`.
    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        self.bytes.reserve(additional)
    }

    #[inline]
    pub fn reserve_exact(&mut self, additional: usize) {
        self.bytes.reserve_exact(additional)
    }

    #[inline]
    pub fn shrink_to_fit(&mut self) {
        self.bytes.shrink_to_fit()
    }

    /// Returns the number of bytes that this string buffer can hold without reallocating.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.bytes.capacity()
    }

    /// Append a UTF-8 slice at the end of the string.
    #[inline]
    pub fn push_str(&mut self, other: &str) {
        self.bytes.extend_from_slice(other.as_bytes())
    }

    /// Append a WTF-8 slice at the end of the string.
    ///
    /// This replaces newly paired surrogates at the boundary
    /// with a supplementary code point,
    /// like concatenating ill-formed UTF-16 strings effectively would.
    #[inline]
    pub fn push_wtf8(&mut self, other: &Wtf8) {
        match ((&*self).final_lead_surrogate(), other.initial_trail_surrogate()) {
            // Replace newly paired surrogates by a supplementary code point.
            (Some(lead), Some(trail)) => {
                let len_without_lead_surrogate = self.len() - 3;
                self.bytes.truncate(len_without_lead_surrogate);
                let other_without_trail_surrogate = &other.bytes[3..];
                // 4 bytes for the supplementary code point
                self.bytes.reserve(4 + other_without_trail_surrogate.len());
                self.push_char(decode_surrogate_pair(lead, trail));
                self.bytes.extend_from_slice(other_without_trail_surrogate);
            }
            _ => self.bytes.extend_from_slice(&other.bytes)
        }
    }

    /// Append a Unicode scalar value at the end of the string.
    #[inline]
    pub fn push_char(&mut self, c: char) {
        self.push_code_point_unchecked(CodePoint::from_char(c))
    }

    /// Append a code point at the end of the string.
    ///
    /// This replaces newly paired surrogates at the boundary
    /// with a supplementary code point,
    /// like concatenating ill-formed UTF-16 strings effectively would.
    #[inline]
    pub fn push(&mut self, code_point: CodePoint) {
        if let trail @ 0xDC00...0xDFFF = code_point.to_u32() {
            if let Some(lead) = (&*self).final_lead_surrogate() {
                let len_without_lead_surrogate = self.len() - 3;
                self.bytes.truncate(len_without_lead_surrogate);
                self.push_char(decode_surrogate_pair(lead, trail as u16));
                return
            }
        }

        // No newly paired surrogates at the boundary.
        self.push_code_point_unchecked(code_point)
    }

    /// Shortens a string to the specified length.
    ///
    /// # Panics
    ///
    /// Panics if `new_len` > current length,
    /// or if `new_len` is not a code point boundary.
    #[inline]
    pub fn truncate(&mut self, new_len: usize) {
        assert!(is_code_point_boundary(self, new_len));
        self.bytes.truncate(new_len)
    }

    /// Consumes the WTF-8 string and tries to convert it to UTF-8.
    ///
    /// This does not copy the data.
    ///
    /// If the contents are not well-formed UTF-8
    /// (that is, if the string contains surrogates),
    /// the original WTF-8 string is returned instead.
    pub fn into_string(self) -> Result<String, Wtf8Buf> {
        match self.next_surrogate(0) {
            None => Ok(unsafe { String::from_utf8_unchecked(self.bytes) }),
            Some(_) => Err(self),
        }
    }

    /// Consumes the WTF-8 string and converts it lossily to UTF-8.
    ///
    /// This does not copy the data (but may overwrite parts of it in place).
    ///
    /// Surrogates are replaced with `"\u{FFFD}"` (the replacement character “�”)
    pub fn into_string_lossy(mut self) -> String {
        let mut pos = 0;
        loop {
            match self.next_surrogate(pos) {
                Some((surrogate_pos, _)) => {
                    pos = surrogate_pos + 3;
                    self.bytes[surrogate_pos..pos]
                        .copy_from_slice(UTF8_REPLACEMENT_CHARACTER.as_bytes());
                },
                None => return unsafe { String::from_utf8_unchecked(self.bytes) }
            }
        }
    }

    /// Converts this `Wtf8Buf` into a boxed `Wtf8`.
    #[inline]
    pub fn into_box(self) -> Box<Wtf8> {
        unsafe { mem::transmute(self.bytes.into_boxed_slice()) }
    }

    /// Converts a `Box<Wtf8>` into a `Wtf8Buf`.
    pub fn from_box(boxed: Box<Wtf8>) -> Wtf8Buf {
        let bytes: Box<[u8]> = unsafe { mem::transmute(boxed) };
        Wtf8Buf { bytes: bytes.into_vec() }
    }
}

/// Create a new WTF-8 string from an iterator of code points.
///
/// This replaces surrogate code point pairs with supplementary code points,
/// like concatenating ill-formed UTF-16 strings effectively would.
impl FromIterator<CodePoint> for Wtf8Buf {
    fn from_iter<T: IntoIterator<Item=CodePoint>>(iter: T) -> Wtf8Buf {
        let mut string = Wtf8Buf::new();
        string.extend(iter);
        string
    }
}

/// Append code points from an iterator to the string.
///
/// This replaces surrogate code point pairs with supplementary code points,
/// like concatenating ill-formed UTF-16 strings effectively would.
impl Extend<CodePoint> for Wtf8Buf {
    fn extend<T: IntoIterator<Item=CodePoint>>(&mut self, iter: T) {
        let iterator = iter.into_iter();
        let (low, _high) = iterator.size_hint();
        // Lower bound of one byte per code point (ASCII only)
        self.bytes.reserve(low);
        for code_point in iterator {
            self.push(code_point);
        }
    }
}

/// A borrowed slice of well-formed WTF-8 data.
///
/// Similar to `&str`, but can additionally contain surrogate code points
/// if they’re not in a surrogate pair.
#[derive(Eq, Ord, PartialEq, PartialOrd)]
pub struct Wtf8 {
    bytes: [u8]
}

impl AsInner<[u8]> for Wtf8 {
    fn as_inner(&self) -> &[u8] { &self.bytes }
}

/// Format the slice with double quotes,
/// and surrogates as `\u` followed by four hexadecimal digits.
/// Example: `"a\u{D800}"` for a slice with code points [U+0061, U+D800]
impl fmt::Debug for Wtf8 {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        fn write_str_escaped(f: &mut fmt::Formatter, s: &str) -> fmt::Result {
            use fmt::Write;
            for c in s.chars().flat_map(|c| c.escape_debug()) {
                f.write_char(c)?
            }
            Ok(())
        }

        formatter.write_str("\"")?;
        let mut pos = 0;
        loop {
            match self.next_surrogate(pos) {
                None => break,
                Some((surrogate_pos, surrogate)) => {
                    write_str_escaped(
                        formatter,
                        unsafe { str::from_utf8_unchecked(
                            &self.bytes[pos .. surrogate_pos]
                        )},
                    )?;
                    write!(formatter, "\\u{{{:x}}}", surrogate)?;
                    pos = surrogate_pos + 3;
                }
            }
        }
        write_str_escaped(
            formatter,
            unsafe { str::from_utf8_unchecked(&self.bytes[pos..]) },
        )?;
        formatter.write_str("\"")
    }
}

impl fmt::Display for Wtf8 {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        let wtf8_bytes = &self.bytes;
        let mut pos = 0;
        loop {
            match self.next_surrogate(pos) {
                Some((surrogate_pos, _)) => {
                    formatter.write_str(unsafe {
                        str::from_utf8_unchecked(&wtf8_bytes[pos .. surrogate_pos])
                    })?;
                    formatter.write_str(UTF8_REPLACEMENT_CHARACTER)?;
                    pos = surrogate_pos + 3;
                },
                None => {
                    let s = unsafe {
                        str::from_utf8_unchecked(&wtf8_bytes[pos..])
                    };
                    if pos == 0 {
                        return s.fmt(formatter)
                    } else {
                        return formatter.write_str(s)
                    }
                }
            }
        }
    }
}


impl Wtf8 {
    /// Creates a WTF-8 slice from a UTF-8 `&str` slice.
    ///
    /// Since WTF-8 is a superset of UTF-8, this always succeeds.
    #[inline]
    pub fn from_str(value: &str) -> &Wtf8 {
        unsafe { Wtf8::from_bytes_unchecked(value.as_bytes()) }
    }

    /// Creates a WTF-8 slice from a WTF-8 byte slice.
    ///
    /// Since the byte slice is not checked for valid WTF-8, this functions is
    /// marked unsafe.
    #[inline]
    unsafe fn from_bytes_unchecked(value: &[u8]) -> &Wtf8 {
        mem::transmute(value)
    }

    /// Returns the length, in WTF-8 bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Returns the code point at `position` if it is in the ASCII range,
    /// or `b'\xFF' otherwise.
    ///
    /// # Panics
    ///
    /// Panics if `position` is beyond the end of the string.
    #[inline]
    pub fn ascii_byte_at(&self, position: usize) -> u8 {
        match self.bytes[position] {
            ascii_byte @ 0x00 ... 0x7F => ascii_byte,
            _ => 0xFF
        }
    }

    /// Returns an iterator for the string’s code points.
    #[inline]
    pub fn code_points(&self) -> Wtf8CodePoints {
        Wtf8CodePoints { bytes: self.bytes.iter() }
    }

    /// Tries to convert the string to UTF-8 and return a `&str` slice.
    ///
    /// Returns `None` if the string contains surrogates.
    ///
    /// This does not copy the data.
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        // Well-formed WTF-8 is also well-formed UTF-8
        // if and only if it contains no surrogate.
        match self.next_surrogate(0) {
            None => Some(unsafe { str::from_utf8_unchecked(&self.bytes) }),
            Some(_) => None,
        }
    }

    /// Lossily converts the string to UTF-8.
    /// Returns a UTF-8 `&str` slice if the contents are well-formed in UTF-8.
    ///
    /// Surrogates are replaced with `"\u{FFFD}"` (the replacement character “�”).
    ///
    /// This only copies the data if necessary (if it contains any surrogate).
    pub fn to_string_lossy(&self) -> Cow<str> {
        let surrogate_pos = match self.next_surrogate(0) {
            None => return Cow::Borrowed(unsafe { str::from_utf8_unchecked(&self.bytes) }),
            Some((pos, _)) => pos,
        };
        let wtf8_bytes = &self.bytes;
        let mut utf8_bytes = Vec::with_capacity(self.len());
        utf8_bytes.extend_from_slice(&wtf8_bytes[..surrogate_pos]);
        utf8_bytes.extend_from_slice(UTF8_REPLACEMENT_CHARACTER.as_bytes());
        let mut pos = surrogate_pos + 3;
        loop {
            match self.next_surrogate(pos) {
                Some((surrogate_pos, _)) => {
                    utf8_bytes.extend_from_slice(&wtf8_bytes[pos .. surrogate_pos]);
                    utf8_bytes.extend_from_slice(UTF8_REPLACEMENT_CHARACTER.as_bytes());
                    pos = surrogate_pos + 3;
                },
                None => {
                    utf8_bytes.extend_from_slice(&wtf8_bytes[pos..]);
                    return Cow::Owned(unsafe { String::from_utf8_unchecked(utf8_bytes) })
                }
            }
        }
    }

    /// Converts the WTF-8 string to potentially ill-formed UTF-16
    /// and return an iterator of 16-bit code units.
    ///
    /// This is lossless:
    /// calling `Wtf8Buf::from_ill_formed_utf16` on the resulting code units
    /// would always return the original WTF-8 string.
    #[inline]
    pub fn encode_wide(&self) -> EncodeWide {
        EncodeWide { code_points: self.code_points(), extra: 0 }
    }

    #[inline]
    fn next_surrogate(&self, mut pos: usize) -> Option<(usize, u16)> {
        let mut iter = self.bytes[pos..].iter();
        loop {
            let b = match iter.next() {
                None => return None,
                Some(&b) => b,
            };
            if b < 0x80 {
                pos += 1;
            } else if b < 0xE0 {
                iter.next();
                pos += 2;
            } else if b == 0xED {
                match (iter.next(), iter.next()) {
                    (Some(&b2), Some(&b3)) if b2 >= 0xA0 => {
                        return Some((pos, decode_surrogate(b2, b3)))
                    }
                    _ => pos += 3
                }
            } else if b < 0xF0 {
                iter.next();
                iter.next();
                pos += 3;
            } else {
                iter.next();
                iter.next();
                iter.next();
                pos += 4;
            }
        }
    }

    #[inline]
    fn final_lead_surrogate(&self) -> Option<u16> {
        let len = self.len();
        if len < 3 {
            return None
        }
        match &self.bytes[(len - 3)..] {
            &[0xED, b2 @ 0xA0...0xAF, b3] => Some(decode_surrogate(b2, b3)),
            _ => None
        }
    }

    #[inline]
    fn initial_trail_surrogate(&self) -> Option<u16> {
        let len = self.len();
        if len < 3 {
            return None
        }
        match &self.bytes[..3] {
            &[0xED, b2 @ 0xB0...0xBF, b3] => Some(decode_surrogate(b2, b3)),
            _ => None
        }
    }

    /// Boxes this `Wtf8`.
    #[inline]
    pub fn into_box(&self) -> Box<Wtf8> {
        let boxed: Box<[u8]> = self.bytes.into();
        unsafe { mem::transmute(boxed) }
    }

    /// Creates a boxed, empty `Wtf8`.
    pub fn empty_box() -> Box<Wtf8> {
        let boxed: Box<[u8]> = Default::default();
        unsafe { mem::transmute(boxed) }
    }
}


/// Return a slice of the given string for the byte range [`begin`..`end`).
///
/// # Panics
///
/// Panics when `begin` and `end` do not point to code point boundaries,
/// or point beyond the end of the string.
impl ops::Index<ops::Range<usize>> for Wtf8 {
    type Output = Wtf8;

    #[inline]
    fn index(&self, range: ops::Range<usize>) -> &Wtf8 {
        // is_code_point_boundary checks that the index is in [0, .len()]
        if range.start <= range.end &&
           is_code_point_boundary(self, range.start) &&
           is_code_point_boundary(self, range.end) {
            unsafe { slice_unchecked(self, range.start, range.end) }
        } else {
            slice_error_fail(self, range.start, range.end)
        }
    }
}

/// Return a slice of the given string from byte `begin` to its end.
///
/// # Panics
///
/// Panics when `begin` is not at a code point boundary,
/// or is beyond the end of the string.
impl ops::Index<ops::RangeFrom<usize>> for Wtf8 {
    type Output = Wtf8;

    #[inline]
    fn index(&self, range: ops::RangeFrom<usize>) -> &Wtf8 {
        // is_code_point_boundary checks that the index is in [0, .len()]
        if is_code_point_boundary(self, range.start) {
            unsafe { slice_unchecked(self, range.start, self.len()) }
        } else {
            slice_error_fail(self, range.start, self.len())
        }
    }
}

/// Return a slice of the given string from its beginning to byte `end`.
///
/// # Panics
///
/// Panics when `end` is not at a code point boundary,
/// or is beyond the end of the string.
impl ops::Index<ops::RangeTo<usize>> for Wtf8 {
    type Output = Wtf8;

    #[inline]
    fn index(&self, range: ops::RangeTo<usize>) -> &Wtf8 {
        // is_code_point_boundary checks that the index is in [0, .len()]
        if is_code_point_boundary(self, range.end) {
            unsafe { slice_unchecked(self, 0, range.end) }
        } else {
            slice_error_fail(self, 0, range.end)
        }
    }
}

impl ops::Index<ops::RangeFull> for Wtf8 {
    type Output = Wtf8;

    #[inline]
    fn index(&self, _range: ops::RangeFull) -> &Wtf8 {
        self
    }
}

#[inline]
fn decode_surrogate(second_byte: u8, third_byte: u8) -> u16 {
    // The first byte is assumed to be 0xED
    0xD800 | (second_byte as u16 & 0x3F) << 6 | third_byte as u16 & 0x3F
}

#[inline]
fn decode_surrogate_pair(lead: u16, trail: u16) -> char {
    let code_point = 0x10000 + ((((lead - 0xD800) as u32) << 10) | (trail - 0xDC00) as u32);
    unsafe { char::from_u32_unchecked(code_point) }
}

/// Copied from core::str::StrPrelude::is_char_boundary
#[inline]
pub fn is_code_point_boundary(slice: &Wtf8, index: usize) -> bool {
    if index == slice.len() { return true; }
    match slice.bytes.get(index) {
        None => false,
        Some(&b) => b < 128 || b >= 192,
    }
}

/// Copied from core::str::raw::slice_unchecked
#[inline]
pub unsafe fn slice_unchecked(s: &Wtf8, begin: usize, end: usize) -> &Wtf8 {
    // memory layout of an &[u8] and &Wtf8 are the same
    Wtf8::from_bytes_unchecked(slice::from_raw_parts(
        s.bytes.as_ptr().offset(begin as isize),
        end - begin
    ))
}

/// Copied from core::str::raw::slice_error_fail
#[inline(never)]
pub fn slice_error_fail(s: &Wtf8, begin: usize, end: usize) -> ! {
    assert!(begin <= end);
    panic!("index {} and/or {} in `{:?}` do not lie on character boundary",
          begin, end, s);
}

/// Iterator for the code points of a WTF-8 string.
///
/// Created with the method `.code_points()`.
#[derive(Clone)]
pub struct Wtf8CodePoints<'a> {
    bytes: slice::Iter<'a, u8>
}

impl<'a> Iterator for Wtf8CodePoints<'a> {
    type Item = CodePoint;

    #[inline]
    fn next(&mut self) -> Option<CodePoint> {
        next_code_point(&mut self.bytes).map(|c| CodePoint { value: c })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.bytes.len();
        (len.saturating_add(3) / 4, Some(len))
    }
}

/// Generates a wide character sequence for potentially ill-formed UTF-16.
#[derive(Clone)]
pub struct EncodeWide<'a> {
    code_points: Wtf8CodePoints<'a>,
    extra: u16
}

// Copied from libunicode/u_str.rs
impl<'a> Iterator for EncodeWide<'a> {
    type Item = u16;

    #[inline]
    fn next(&mut self) -> Option<u16> {
        if self.extra != 0 {
            let tmp = self.extra;
            self.extra = 0;
            return Some(tmp);
        }

        let mut buf = [0; 2];
        self.code_points.next().map(|code_point| {
            let c = unsafe {
                char::from_u32_unchecked(code_point.value)
            };
            let n = c.encode_utf16(&mut buf).len();
            if n == 2 {
                self.extra = buf[1];
            }
            buf[0]
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let (low, high) = self.code_points.size_hint();
        // every code point gets either one u16 or two u16,
        // so this iterator is between 1 or 2 times as
        // long as the underlying iterator.
        (low, high.and_then(|n| n.checked_mul(2)))
    }
}

impl Hash for CodePoint {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state)
    }
}

impl Hash for Wtf8Buf {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&self.bytes);
        0xfeu8.hash(state)
    }
}

impl Hash for Wtf8 {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&self.bytes);
        0xfeu8.hash(state)
    }
}

impl AsciiExt for Wtf8 {
    type Owned = Wtf8Buf;

    fn is_ascii(&self) -> bool {
        self.bytes.is_ascii()
    }
    fn to_ascii_uppercase(&self) -> Wtf8Buf {
        Wtf8Buf { bytes: self.bytes.to_ascii_uppercase() }
    }
    fn to_ascii_lowercase(&self) -> Wtf8Buf {
        Wtf8Buf { bytes: self.bytes.to_ascii_lowercase() }
    }
    fn eq_ignore_ascii_case(&self, other: &Wtf8) -> bool {
        self.bytes.eq_ignore_ascii_case(&other.bytes)
    }

    fn make_ascii_uppercase(&mut self) { self.bytes.make_ascii_uppercase() }
    fn make_ascii_lowercase(&mut self) { self.bytes.make_ascii_lowercase() }
}
