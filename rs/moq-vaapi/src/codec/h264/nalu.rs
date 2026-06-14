// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::borrow::Cow;
use std::fmt::Debug;
use std::io::Cursor;
use std::io::Seek;
use std::io::SeekFrom;

#[allow(clippy::len_without_is_empty)]
pub trait Header: Sized {
    /// Parse the NALU header, returning it.
    fn parse<T: AsRef<[u8]>>(cursor: &mut Cursor<T>) -> Result<Self, String>;
    /// Whether this header type indicates EOS.
    fn is_end(&self) -> bool;
    /// The length of the header.
    fn len(&self) -> usize;
}

#[derive(Debug)]
pub struct Nalu<'a, U> {
    pub header: U,
    /// The mapping that backs this NALU. Possibly shared with the other NALUs
    /// in the Access Unit.
    pub data: Cow<'a, [u8]>,

    pub size: usize,
    pub offset: usize,
}

impl<'a, U> Nalu<'a, U>
where
    U: Debug + Header,
{
    /// Find the next Annex B encoded NAL unit.
    pub fn next(cursor: &mut Cursor<&'a [u8]>) -> Result<Nalu<'a, U>, String> {
        let bitstream = cursor.clone().into_inner();
        let pos = usize::try_from(cursor.position()).map_err(|err| err.to_string())?;

        // Find the start code for this NALU
        let current_nalu_offset = match Nalu::<'a, U>::find_start_code(cursor, pos) {
            Some(offset) => offset,
            None => return Err("No NAL found".into()),
        };

        let mut start_code_offset = pos + current_nalu_offset;

        // If the preceding byte is 00, then we actually have a four byte SC,
        // i.e. 00 00 00 01 Where the first 00 is the "zero_byte()"
        if start_code_offset > 0 && cursor.get_ref()[start_code_offset - 1] == 00 {
            start_code_offset -= 1;
        }

        // The NALU offset is its offset + 3 bytes to skip the start code.
        let nalu_offset = pos + current_nalu_offset + 3;

        // Set the bitstream position to the start of the current NALU
        cursor.set_position(u64::try_from(nalu_offset).map_err(|err| err.to_string())?);

        let hdr = U::parse(cursor)?;

        // Find the start of the subsequent NALU.
        let mut next_nalu_offset = match Nalu::<'a, U>::find_start_code(cursor, nalu_offset) {
            Some(offset) => offset,
            None => {
                let cur_pos = cursor.position();
                let end_pos = cursor.seek(SeekFrom::End(0)).map_err(|err| err.to_string())?;
                let _ = cursor.seek(SeekFrom::Start(cur_pos)).map_err(|err| err.to_string())?;
                (end_pos - cur_pos) as usize
            } // Whatever data is left must be part of the current NALU
        };

        while next_nalu_offset > 0 && cursor.get_ref()[nalu_offset + next_nalu_offset - 1] == 00 {
            // Discard trailing_zero_8bits
            next_nalu_offset -= 1;
        }

        let nal_size = if hdr.is_end() {
            // the NALU is comprised of only the header
            hdr.len()
        } else {
            next_nalu_offset
        };

        Ok(Nalu {
            header: hdr,
            data: Cow::from(&bitstream[start_code_offset..nalu_offset + nal_size]),
            size: nal_size,
            offset: nalu_offset - start_code_offset,
        })
    }
}

impl<'a, U> Nalu<'a, U>
where
    U: Debug,
{
    fn find_start_code(data: &mut Cursor<&'a [u8]>, offset: usize) -> Option<usize> {
        // discard all zeroes until the start code pattern is found
        data.get_ref()[offset..].windows(3).position(|window| window == [0x00, 0x00, 0x01])
    }

    pub fn into_owned(self) -> Nalu<'static, U> {
        Nalu {
            header: self.header,
            size: self.size,
            offset: self.offset,
            data: Cow::Owned(self.data.into_owned()),
        }
    }
}

impl<'a, U> AsRef<[u8]> for Nalu<'a, U> {
    fn as_ref(&self) -> &[u8] {
        &self.data[self.offset..self.offset + self.size]
    }
}
