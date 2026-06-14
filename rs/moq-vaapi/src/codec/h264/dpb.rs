// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::Ref;
use std::cell::RefMut;
use std::fmt;
use std::rc::Rc;

use log::debug;

use crate::codec::h264::parser::MaxLongTermFrameIdx;
use crate::codec::h264::parser::RefPicMarkingInner;
use crate::codec::h264::parser::Sps;
use crate::codec::h264::picture::Field;
use crate::codec::h264::picture::FieldRank;
use crate::codec::h264::picture::IsIdr;
use crate::codec::h264::picture::PictureData;
use crate::codec::h264::picture::RcPictureData;
use crate::codec::h264::picture::Reference;

pub type DpbPicRefList<'a, H> = Vec<&'a DpbEntry<H>>;

/// All the reference picture lists used to decode a picture.
#[derive(Default)]
pub struct ReferencePicLists {
    /// Reference picture list for P slices. Retains the same meaning as in the
    /// specification. Points into the pictures stored in the DPB. Derived once
    /// per picture.
    pub ref_pic_list_p0: Vec<usize>,
    /// Reference picture list 0 for B slices. Retains the same meaning as in
    /// the specification. Points into the pictures stored in the DPB. Derived
    /// once per picture.
    pub ref_pic_list_b0: Vec<usize>,
    /// Reference picture list 1 for B slices. Retains the same meaning as in
    /// the specification. Points into the pictures stored in the DPB. Derived
    /// once per picture.
    pub ref_pic_list_b1: Vec<usize>,
}

/// A single entry in the DPB.
#[derive(Clone)]
pub struct DpbEntry<T> {
    /// `PictureData` of the frame in this entry.
    pub pic: RcPictureData,
    /// Reference to the decoded frame, ensuring that it doesn't get reused while in the DPB.
    pub reference: Option<T>,
    /// Decoded frame promise. It will be set when the frame enters the DPB, and taken during the
    /// bump process.
    pub decoded_frame: Option<T>,
    /// Whether the picture is still waiting to be bumped and displayed.
    needed_for_output: bool,
}

impl<T> DpbEntry<T> {
    /// Returns `true` is the entry is eligible to be bumped.
    ///
    /// An entry can be bumped if its `needed_for_output` flag is true and it is the first field of
    /// a frame which fields are all decoded.
    fn is_bumpable(&self) -> bool {
        if !self.needed_for_output {
            return false;
        }

        let pic = self.pic.borrow();
        match pic.field {
            // Progressive frames in the DPB are fully decoded.
            Field::Frame => true,
            // Only return the first field of fully decoded interlaced frames.
            Field::Top | Field::Bottom => matches!(pic.field_rank(), FieldRank::First(..)),
        }
    }
}

pub struct Dpb<T> {
    /// List of `PictureData` and backend handles to decoded pictures.
    entries: Vec<DpbEntry<T>>,
    /// The maximum number of pictures that can be stored.
    max_num_pics: usize,
    /// Indicates an upper bound for the number of frames buffers, in the
    /// decoded picture buffer (DPB), that are required for storing frames,
    /// complementary field pairs, and non-paired fields before output. It is a
    /// requirement of bitstream conformance that the maximum number of frames,
    /// complementary field pairs, or non-paired fields that precede any frame,
    /// complementary field pair, or non-paired field in the coded video
    /// sequence in decoding order and follow it in output order shall be less
    /// than or equal to max_num_reorder_frames.
    max_num_reorder_frames: usize,
    /// Whether we're decoding in interlaced mode. Interlaced support is
    /// inspired by the GStreamer implementation, in which frames are split if
    /// interlaced=1. This makes reference marking easier. We also decode both
    /// fields to the same frame, and this frame with both fields is outputted
    /// only once.
    interlaced: bool,
}

#[derive(Debug)]
pub enum StorePictureError {
    DpbIsFull,
}

impl fmt::Display for StorePictureError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "DPB is full")
    }
}

impl std::error::Error for StorePictureError {}

#[derive(Debug)]
pub enum MmcoError {
    NoShortTermPic,
    ExpectedMarked,
    ExpectedExisting,
    UnknownMmco(u8),
}

impl fmt::Display for MmcoError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MmcoError::NoShortTermPic => {
                write!(f, "could not find ShortTerm picture to mark in the DPB")
            }
            MmcoError::ExpectedMarked => {
                write!(f, "a ShortTerm picture was expected to be marked for MMCO=3")
            }
            MmcoError::ExpectedExisting => {
                write!(f, "picture cannot be marked as nonexisting for MMCO=3")
            }
            MmcoError::UnknownMmco(x) => write!(f, "unknown MMCO: {}", x),
        }
    }
}

impl std::error::Error for MmcoError {}

impl<T: Clone> Dpb<T> {
    /// Returns an iterator over the underlying H264 pictures stored in the
    /// DPB.
    fn pictures(&self) -> impl Iterator<Item = Ref<'_, PictureData>> {
        self.entries.iter().map(|h| h.pic.borrow())
    }

    /// Returns a mutable iterator over the underlying H264 pictures stored in
    /// the DPB.
    fn pictures_mut(&mut self) -> impl Iterator<Item = RefMut<'_, PictureData>> {
        self.entries.iter().map(|h| h.pic.borrow_mut())
    }

    /// Returns the length of the DPB.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a reference to the whole DPB entries.
    pub fn entries(&self) -> &Vec<DpbEntry<T>> {
        &self.entries
    }

    /// Set the DPB's limits in terms of maximum number or pictures.
    pub fn set_limits(&mut self, max_num_pics: usize, max_num_reorder_frames: usize) {
        self.max_num_pics = max_num_pics;
        self.max_num_reorder_frames = max_num_reorder_frames;
    }

    /// Get a reference to the dpb's max num pics.
    pub fn max_num_pics(&self) -> usize {
        self.max_num_pics
    }

    // Returns the number of reference frames, counting the first field only if
    // dealing with interlaced content.
    pub fn num_ref_frames(&self) -> usize {
        self.pictures().filter(|p| p.is_ref() && !p.is_second_field()).count()
    }

    /// Get a reference to the dpb's interlaced mode.
    pub fn interlaced(&self) -> bool {
        self.interlaced
    }

    /// Set the dpb's interlaced mode.
    pub fn set_interlaced(&mut self, interlaced: bool) {
        self.interlaced = interlaced;
    }

    /// Find the short term reference picture with the lowest `frame_num_wrap`
    /// value.
    pub fn find_short_term_lowest_frame_num_wrap(&self) -> Option<&DpbEntry<T>> {
        let lowest = self
            .entries
            .iter()
            .filter(|h| {
                let p = h.pic.borrow();
                matches!(p.reference(), Reference::ShortTerm)
            })
            .min_by_key(|h| {
                let p = h.pic.borrow();
                p.frame_num_wrap
            });

        lowest
    }

    /// Mark all pictures in the DPB as unused for reference.
    pub fn mark_all_as_unused_for_ref(&mut self) {
        for mut picture in self.pictures_mut() {
            picture.set_reference(Reference::None, false);
        }
    }

    /// Remove unused pictures from the DPB. A picture is not going to be used
    /// anymore if it's a) not a reference and b) not needed for output
    fn remove_unused(&mut self) {
        self.entries.retain(|entry| {
            let pic = entry.pic.borrow();
            let discard = !pic.is_ref() && !entry.needed_for_output;

            if discard {
                log::debug!("Removing unused picture {:#?}", pic);
            }

            !discard
        });
    }

    /// Find a short term reference picture with the given `pic_num` value.
    fn find_short_term_with_pic_num_pos(&self, pic_num: i32) -> Option<usize> {
        let position = self
            .pictures()
            .position(|p| matches!(p.reference(), Reference::ShortTerm) && p.pic_num == pic_num);

        log::debug!("find_short_term_with_pic_num: {}, found position {:?}", pic_num, position);

        position
    }

    /// Find a short term reference picture with the given `pic_num` value.
    pub fn find_short_term_with_pic_num(&self, pic_num: i32) -> Option<&DpbEntry<T>> {
        let position = self.find_short_term_with_pic_num_pos(pic_num)?;
        Some(&self.entries[position])
    }

    /// Find a long term reference picture with the given `long_term_pic_num`
    /// value.
    fn find_long_term_with_long_term_pic_num_pos(&self, long_term_pic_num: u32) -> Option<usize> {
        let position = self.pictures().position(|p| {
            matches!(p.reference(), Reference::LongTerm) && p.long_term_pic_num == long_term_pic_num
        });

        log::debug!(
            "find_long_term_with_long_term_pic_num: {}, found position {:?}",
            long_term_pic_num,
            position
        );

        position
    }

    /// Find a long term reference picture with the given `long_term_pic_num`
    /// value.
    pub fn find_long_term_with_long_term_pic_num(
        &self,
        long_term_pic_num: u32,
    ) -> Option<&DpbEntry<T>> {
        let position = self.find_long_term_with_long_term_pic_num_pos(long_term_pic_num)?;
        Some(&self.entries[position])
    }

    /// Store `picture` and its backend handle in the DPB.
    pub fn store_picture(
        &mut self,
        picture: RcPictureData,
        handle: Option<T>,
    ) -> Result<(), StorePictureError> {
        let max_pics = if self.interlaced { self.max_num_pics * 2 } else { self.max_num_pics };

        if self.entries.len() >= max_pics {
            return Err(StorePictureError::DpbIsFull);
        }

        let pic = picture.borrow();

        // C.4.2. Decoding of gaps in frame_num and storage of "non-existing"
        // pictures
        let needed_for_output = !pic.nonexisting;

        debug!(
            "Stored picture POC {:?}, field {:?}, the DPB length is {:?}",
            pic.pic_order_cnt,
            pic.field,
            self.entries.len()
        );
        drop(pic);

        self.entries.push(DpbEntry {
            pic: picture,
            reference: handle.clone(),
            decoded_frame: handle,
            needed_for_output,
        });

        Ok(())
    }

    /// Whether the DPB has an empty slot for a new picture.
    pub fn has_empty_frame_buffer(&self) -> bool {
        let count = if !self.interlaced {
            self.entries.len()
        } else {
            self.pictures()
                .filter(|pic| {
                    matches!(pic.field_rank(), FieldRank::First(..))
                        || (matches!(pic.field_rank(), FieldRank::Single)
                            && pic.field == Field::Frame)
                })
                .count()
        };

        count < self.max_num_pics
    }

    /// Whether the DPB needs bumping, as described by clauses 1, 4, 5, 6 of
    /// C.4.5.3 "Bumping" process.
    pub fn needs_bumping(&self, to_insert: &PictureData) -> bool {
        // In C.4.5.3 we handle clauses 2 and 3 separately. All other clauses
        // check for an empty frame buffer first. Here we handle:
        //    - There is no empty frame buffer and a empty frame buffer is
        //    needed for storage of an inferred "non-existing" frame.
        //
        //    - There is no empty frame buffer and an empty frame buffer is
        //    needed for storage of a decoded (non-IDR) reference picture.
        //
        //    - There is no empty frame buffer and the current picture is a non-
        //    reference picture that is not the second field of a complementary
        //    non-reference field pair and there are pictures in the DPB that
        //    are marked as "needed for output" that precede the current
        //    non-reference picture in output order.
        //
        // Clauses 2 and 3 are handled by H264Codec::handle_picture and
        // H264Codec::finish_picture, respectively.
        if self.has_empty_frame_buffer() {
            return false;
        }

        if to_insert.nonexisting {
            return true;
        }

        let non_idr_ref = to_insert.is_ref() && matches!(to_insert.is_idr, IsIdr::No);
        if non_idr_ref {
            return true;
        }

        let lowest_poc = match self.find_lowest_poc_for_bumping() {
            Some(handle) => handle.pic.borrow().pic_order_cnt,
            None => return false,
        };

        !to_insert.is_second_field_of_complementary_ref_pair()
            && to_insert.pic_order_cnt > lowest_poc
    }

    /// Find the lowest POC in the DPB that can be bumped.
    fn find_lowest_poc_for_bumping(&self) -> Option<&DpbEntry<T>> {
        self.entries
            .iter()
            .filter(|entry| entry.is_bumpable())
            .min_by_key(|handle| handle.pic.borrow().pic_order_cnt)
    }

    /// Find the lowest POC in the DPB that can be bumped and return a mutable reference.
    fn find_lowest_poc_for_bumping_mut(&mut self) -> Option<&mut DpbEntry<T>> {
        self.entries
            .iter_mut()
            .filter(|entry| entry.is_bumpable())
            .min_by_key(|handle| handle.pic.borrow().pic_order_cnt)
    }

    /// Bump the dpb, returning a picture as per the bumping process described in C.4.5.3.
    /// Note that this picture will still be referenced by its pair, if any.
    fn bump(&mut self) -> Option<Option<T>> {
        let dpb_entry = self.find_lowest_poc_for_bumping_mut()?;
        let handle = dpb_entry.decoded_frame.take();
        let pic = dpb_entry.pic.borrow();

        debug!("Bumping picture {:#?} from the dpb", pic);

        dpb_entry.needed_for_output = false;
        // Lookup the second field entry and flip as well.
        // `find_lowest_poc_for_bumping_mut` always returns the first field, never the second.
        if let FieldRank::First(second_field) = pic.field_rank() {
            let second_field = second_field.upgrade();
            drop(pic);
            if let Some(second_field) =
                second_field.and_then(|f| self.entries.iter_mut().find(|e| Rc::ptr_eq(&f, &e.pic)))
            {
                second_field.needed_for_output = false;
            }
        }

        Some(handle)
    }

    /// Drains the DPB by continuously invoking the bumping process.
    pub fn drain(&mut self) -> Vec<Option<T>> {
        debug!("Draining the DPB.");

        let mut pics = vec![];

        while let Some(pic) = self.bump() {
            pics.push(pic);
        }

        self.clear();

        pics
    }

    /// Clears the DPB, dropping all the pictures.
    pub fn clear(&mut self) {
        debug!("Clearing the DPB");

        let max_num_pics = self.max_num_pics;
        let interlaced = self.interlaced;

        *self = Default::default();

        self.max_num_pics = max_num_pics;
        self.interlaced = interlaced;
    }

    /// Returns an iterator of short term refs.
    pub fn short_term_refs_iter(&self) -> impl Iterator<Item = &DpbEntry<T>> {
        self.entries
            .iter()
            .filter(|&handle| matches!(handle.pic.borrow().reference(), Reference::ShortTerm))
    }

    /// Returns an iterator of long term refs.
    pub fn long_term_refs_iter(&self) -> impl Iterator<Item = &DpbEntry<T>> {
        self.entries
            .iter()
            .filter(|&handle| matches!(handle.pic.borrow().reference(), Reference::LongTerm))
    }

    pub fn update_pic_nums(
        &mut self,
        frame_num: u32,
        max_frame_num: u32,
        current_pic: &PictureData,
    ) {
        for mut pic in self.pictures_mut() {
            if !pic.is_ref() {
                continue;
            }

            if *pic.reference() == Reference::LongTerm {
                pic.long_term_pic_num = if current_pic.field == Field::Frame {
                    pic.long_term_frame_idx
                } else if current_pic.field == pic.field {
                    2 * pic.long_term_frame_idx + 1
                } else {
                    2 * pic.long_term_frame_idx
                };
            } else {
                pic.frame_num_wrap = if pic.frame_num > frame_num {
                    pic.frame_num as i32 - max_frame_num as i32
                } else {
                    pic.frame_num as i32
                };

                pic.pic_num = if current_pic.field == Field::Frame {
                    pic.frame_num_wrap
                } else if pic.field == current_pic.field {
                    2 * pic.frame_num_wrap + 1
                } else {
                    2 * pic.frame_num_wrap
                };
            }
        }
    }

    /// Bumps the DPB if needed. DPB bumping is described on C.4.5.3.
    pub fn bump_as_needed(&mut self, current_pic: &PictureData) -> Vec<Option<T>> {
        let mut pics = vec![];
        while self.needs_bumping(current_pic) && self.len() >= self.max_num_reorder_frames {
            match self.bump() {
                Some(pic) => pics.push(pic),
                None => return pics,
            }
            self.remove_unused();
        }

        pics
    }

    // 8.2.5.3
    pub fn sliding_window_marking(&mut self, pic: &mut PictureData, sps: &Sps) {
        // If the current picture is a coded field that is the second field in
        // decoding order of a complementary reference field pair, and the first
        // field has been marked as "used for short-term reference", the current
        // picture and the complementary reference field pair are also marked as
        // "used for short-term reference".
        if let FieldRank::Second(other_field) = pic.field_rank() {
            if matches!(other_field.borrow().reference(), Reference::ShortTerm) {
                pic.set_reference(Reference::ShortTerm, false);
                return;
            }
        }

        let mut num_ref_pics = self.num_ref_frames();
        let max_num_ref_frames = std::cmp::max(1, sps.max_num_ref_frames as usize);

        if num_ref_pics < max_num_ref_frames {
            return;
        }

        while num_ref_pics >= max_num_ref_frames {
            if let Some(to_unmark) = self.find_short_term_lowest_frame_num_wrap() {
                to_unmark.pic.borrow_mut().set_reference(Reference::None, true);
                num_ref_pics -= 1;
            } else {
                log::warn!("could not find a ShortTerm picture to unmark in the DPB");
                break;
            }
        }

        self.remove_unused();
    }

    pub fn mmco_op_1(
        &mut self,
        pic: &PictureData,
        marking: &RefPicMarkingInner,
    ) -> Result<(), MmcoError> {
        let pic_num_x = pic.pic_num - (marking.difference_of_pic_nums_minus1 as i32 + 1);

        log::debug!("MMCO op 1 for pic_num_x {}", pic_num_x);
        log::trace!("Dpb state before MMCO=1: {:#?}", self);

        let to_mark =
            self.find_short_term_with_pic_num(pic_num_x).ok_or(MmcoError::NoShortTermPic)?;

        to_mark.pic.borrow_mut().set_reference(Reference::None, matches!(pic.field, Field::Frame));

        Ok(())
    }

    pub fn mmco_op_2(
        &mut self,
        pic: &PictureData,
        marking: &RefPicMarkingInner,
    ) -> Result<(), MmcoError> {
        log::debug!("MMCO op 2 for long_term_pic_num {}", marking.long_term_pic_num);

        log::trace!("Dpb state before MMCO=2: {:#?}", self);

        let to_mark = self
            .find_long_term_with_long_term_pic_num(marking.long_term_pic_num)
            .ok_or(MmcoError::NoShortTermPic)?;

        to_mark.pic.borrow_mut().set_reference(Reference::None, matches!(pic.field, Field::Frame));

        Ok(())
    }

    pub fn mmco_op_3(
        &mut self,
        pic: &PictureData,
        marking: &RefPicMarkingInner,
    ) -> Result<(), MmcoError> {
        let pic_num_x = pic.pic_num - (marking.difference_of_pic_nums_minus1 as i32 + 1);

        log::debug!("MMCO op 3 for pic_num_x {}", pic_num_x);
        log::trace!("Dpb state before MMCO=3: {:#?}", self);

        let to_mark_as_long_pos =
            self.find_short_term_with_pic_num_pos(pic_num_x).ok_or(MmcoError::NoShortTermPic)?;
        let to_mark_as_long = &self.entries[to_mark_as_long_pos].pic;

        if !matches!(to_mark_as_long.borrow().reference(), Reference::ShortTerm) {
            return Err(MmcoError::ExpectedMarked);
        }

        if to_mark_as_long.borrow().nonexisting {
            return Err(MmcoError::ExpectedExisting);
        }

        let to_mark_as_long_ptr = to_mark_as_long.as_ptr();
        let to_mark_as_long_other_field_ptr =
            to_mark_as_long.borrow().other_field().map(|f| f.as_ptr());

        let long_term_frame_idx = marking.long_term_frame_idx;

        for mut picture in self.pictures_mut() {
            let long_already_assigned = matches!(picture.reference(), Reference::LongTerm)
                && picture.long_term_frame_idx == long_term_frame_idx;

            if long_already_assigned {
                let is_frame = matches!(picture.field, Field::Frame);

                let is_complementary_field_pair = picture
                    .other_field()
                    .map(|f| {
                        let pic = f.borrow();
                        matches!(pic.reference(), Reference::LongTerm)
                            && pic.long_term_frame_idx == long_term_frame_idx
                    })
                    .unwrap_or(false);

                // When LongTermFrameIdx equal to
                // long_term_frame_idx is already assigned to a
                // long-term reference frame or a long-term
                // complementary reference field pair, that frame or
                // complementary field pair and both of its fields
                // are marked as "unused for reference"
                if is_frame || is_complementary_field_pair {
                    picture.set_reference(Reference::None, true);
                    break;
                }

                // When LongTermFrameIdx is already assigned to a
                // reference field, and that reference field is not
                // part of a complementary field pair that includes
                // the picture specified by picNumX, that field is
                // marked as "unused for reference".
                let reference_field_is_not_part_of_pic_x = match picture.other_field() {
                    None => true,
                    Some(other_field) => {
                        // Check that the fields do not reference one another.
                        !std::ptr::eq(other_field.as_ptr(), to_mark_as_long_ptr)
                            && to_mark_as_long_other_field_ptr
                                .map(|p| !std::ptr::eq(p, &(*picture)))
                                .unwrap_or(true)
                    }
                };

                if reference_field_is_not_part_of_pic_x {
                    picture.set_reference(Reference::None, false);
                    break;
                }
            }
        }

        let is_frame = matches!(pic.field, Field::Frame);
        let to_mark_as_long = &self.entries[to_mark_as_long_pos].pic;
        to_mark_as_long.borrow_mut().set_reference(Reference::LongTerm, is_frame);
        to_mark_as_long.borrow_mut().long_term_frame_idx = long_term_frame_idx;

        if let Some(other_field) = to_mark_as_long.borrow().other_field() {
            let mut other_field = other_field.borrow_mut();
            if matches!(other_field.reference(), Reference::LongTerm) {
                other_field.long_term_frame_idx = long_term_frame_idx;

                log::debug!(
                    "Assigned long_term_frame_idx {} to other_field {:#?}",
                    long_term_frame_idx,
                    &other_field
                );
            }
        }

        Ok(())
    }

    /// Returns the new `max_long_term_frame_idx`.
    pub fn mmco_op_4(&mut self, marking: &RefPicMarkingInner) -> MaxLongTermFrameIdx {
        log::debug!("MMCO op 4, max_long_term_frame_idx: {:?}", marking.max_long_term_frame_idx);

        log::trace!("Dpb state before MMCO=4: {:#?}", self);

        for mut dpb_pic in self
            .pictures_mut()
            .filter(|pic| matches!(pic.reference(), Reference::LongTerm))
            .filter(|pic| marking.max_long_term_frame_idx < pic.long_term_frame_idx)
        {
            dpb_pic.set_reference(Reference::None, false);
        }

        marking.max_long_term_frame_idx
    }

    /// Returns the new `max_long_term_frame_idx`.
    pub fn mmco_op_5(&mut self, pic: &mut PictureData) -> MaxLongTermFrameIdx {
        log::debug!("MMCO op 5, marking all pictures in the DPB as unused for reference");
        log::trace!("Dpb state before MMCO=5: {:#?}", self);

        self.mark_all_as_unused_for_ref();

        pic.has_mmco_5 = true;

        // A picture including a memory_management_control_operation equal to 5
        // shall have frame_num constraints as described above and, after the
        // decoding of the current picture and the processing of the memory
        // management control operations, the picture shall be inferred to have
        // had frame_num equal to 0 for all subsequent use in the decoding
        // process, except as specified in clause 7.4.1.2.4.
        pic.frame_num = 0;

        // When the current picture includes a
        // memory_management_control_operation equal to 5, after the decoding of
        // the current picture, tempPicOrderCnt is set equal to PicOrderCnt(
        // CurrPic ), TopFieldOrderCnt of the current picture (if any) is set
        // equal to TopFieldOrderCnt − tempPicOrderCnt, and BottomFieldOrderCnt
        // of the current picture (if any) is set equal to BottomFieldOrderCnt −
        // tempPicOrderCnt
        match pic.field {
            Field::Top => {
                pic.top_field_order_cnt = 0;
                pic.pic_order_cnt = 0;
            }
            Field::Bottom => {
                pic.bottom_field_order_cnt = 0;
                pic.pic_order_cnt = 0;
            }
            Field::Frame => {
                pic.top_field_order_cnt -= pic.pic_order_cnt;
                pic.bottom_field_order_cnt -= pic.pic_order_cnt;
                pic.pic_order_cnt =
                    std::cmp::min(pic.top_field_order_cnt, pic.bottom_field_order_cnt);
            }
        }

        MaxLongTermFrameIdx::NoLongTermFrameIndices
    }

    pub fn mmco_op_6(&mut self, pic: &mut PictureData, marking: &RefPicMarkingInner) {
        let long_term_frame_idx = marking.long_term_frame_idx;

        log::debug!("MMCO op 6, long_term_frame_idx: {}", long_term_frame_idx);
        log::trace!("Dpb state before MMCO=6: {:#?}", self);

        for mut dpb_pic in self.pictures_mut() {
            // When a variable LongTermFrameIdx equal to long_term_frame_idx is
            // already assigned to a long-term reference frame or a long-term
            // complementary reference field pair, that frame or complementary
            // field pair and both of its fields are marked as "unused for
            // reference". When LongTermFrameIdx is already assigned to a
            // reference field, and that reference field is not part of a
            // complementary field pair that includes the current picture, that
            // field is marked as "unused for reference".
            if matches!(dpb_pic.reference(), Reference::LongTerm)
                && dpb_pic.long_term_frame_idx == long_term_frame_idx
            {
                let is_frame = matches!(dpb_pic.field, Field::Frame);

                let is_complementary_ref_field_pair = dpb_pic
                    .other_field()
                    .map(|f| {
                        let pic = f.borrow();
                        matches!(pic.reference(), Reference::LongTerm)
                            && pic.long_term_frame_idx == long_term_frame_idx
                    })
                    .unwrap_or(false);

                dpb_pic.set_reference(Reference::None, is_frame || is_complementary_ref_field_pair);

                break;
            }
        }

        let is_frame = matches!(pic.field, Field::Frame);

        let is_second_ref_field = match pic.field_rank() {
            FieldRank::Second(first_field)
                if *first_field.borrow().reference() == Reference::LongTerm =>
            {
                first_field.borrow_mut().long_term_frame_idx = long_term_frame_idx;
                true
            }
            _ => false,
        };

        pic.set_reference(Reference::LongTerm, is_frame || is_second_ref_field);
        pic.long_term_frame_idx = long_term_frame_idx;
    }

    #[cfg(debug_assertions)]
    fn debug_ref_list_p(ref_pic_list: &[&DpbEntry<T>], field_pic: bool) {
        debug!(
            "ref_list_p0: (ShortTerm|LongTerm, pic_num) {:?}",
            ref_pic_list
                .iter()
                .map(|h| {
                    let p = h.pic.borrow();
                    let reference = match p.reference() {
                        Reference::None => panic!("Not a reference."),
                        Reference::ShortTerm => "ShortTerm",
                        Reference::LongTerm => "LongTerm",
                    };

                    let field = if !p.is_second_field() { "First field" } else { "Second field" };

                    let field = format!("{}, {:?}", field, p.field);

                    let inner = match (field_pic, p.reference()) {
                        (false, _) => ("pic_num", p.pic_num, field),
                        (true, Reference::ShortTerm) => ("frame_num_wrap", p.frame_num_wrap, field),
                        (true, Reference::LongTerm) => {
                            ("long_term_frame_idx", p.long_term_frame_idx as i32, field)
                        }

                        _ => panic!("Not a reference."),
                    };
                    (reference, inner)
                })
                .collect::<Vec<_>>()
        );
    }

    #[cfg(debug_assertions)]
    fn debug_ref_list_b(ref_pic_list: &[&DpbEntry<T>], ref_pic_list_name: &str) {
        debug!(
            "{:?}: (ShortTerm|LongTerm, (POC|LongTermPicNum)) {:?}",
            ref_pic_list_name,
            ref_pic_list
                .iter()
                .map(|h| {
                    let p = h.pic.borrow();
                    let reference = match p.reference() {
                        Reference::None => panic!("Not a reference."),
                        Reference::ShortTerm => "ShortTerm",
                        Reference::LongTerm => "LongTerm",
                    };

                    let field = if !p.is_second_field() { "First field" } else { "Second field" };

                    let field = format!("{}, {:?}", field, p.field);

                    let inner = match p.reference() {
                        Reference::ShortTerm => ("POC", p.pic_order_cnt, field),
                        Reference::LongTerm => {
                            ("LongTermPicNum", p.long_term_pic_num as i32, field)
                        }
                        _ => panic!("Not a reference!"),
                    };
                    (reference, inner)
                })
                .collect::<Vec<_>>()
        );
    }

    fn sort_pic_num_descending(pics: &mut [&DpbEntry<T>]) {
        pics.sort_by_key(|h| std::cmp::Reverse(h.pic.borrow().pic_num));
    }

    fn sort_frame_num_wrap_descending(pics: &mut [&DpbEntry<T>]) {
        pics.sort_by_key(|h| std::cmp::Reverse(h.pic.borrow().frame_num_wrap));
    }

    fn sort_long_term_pic_num_ascending(pics: &mut [&DpbEntry<T>]) {
        pics.sort_by_key(|h| h.pic.borrow().long_term_pic_num);
    }

    fn sort_long_term_frame_idx_ascending(pics: &mut [&DpbEntry<T>]) {
        pics.sort_by_key(|h| h.pic.borrow().long_term_frame_idx);
    }

    fn sort_poc_descending(pics: &mut [&DpbEntry<T>]) {
        pics.sort_by_key(|h| std::cmp::Reverse(h.pic.borrow().pic_order_cnt));
    }

    fn sort_poc_ascending(pics: &mut [&DpbEntry<T>]) {
        pics.sort_by_key(|h| h.pic.borrow().pic_order_cnt);
    }

    // When the reference picture list RefPicList1 has more than one entry
    // and RefPicList1 is identical to the reference picture list
    // RefPicList0, the first two entries RefPicList1[0] and RefPicList1[1]
    // are switched.
    fn swap_b1_if_needed(b0: &DpbPicRefList<T>, b1: &mut DpbPicRefList<T>) {
        if b1.len() > 1 && b0.len() == b1.len() {
            let mut equals = true;
            for (x1, x2) in b0.iter().zip(b1.iter()) {
                if !Rc::ptr_eq(&x1.pic, &x2.pic) {
                    equals = false;
                    break;
                }
            }

            if equals {
                b1.swap(0, 1);
            }
        }
    }

    /// Copies from refFrameList(XShort|Long)Term into RefPicListX as per 8.2.4.2.5. Used when
    /// building the reference list for fields in interlaced decoding.
    fn init_ref_field_pic_list<'a>(
        mut field: Field,
        reference_type: Reference,
        ref_frame_list: &mut DpbPicRefList<'a, T>,
        ref_pic_list: &mut DpbPicRefList<'a, T>,
    ) {
        // When one field of a reference frame was not decoded or is not marked as "used for
        // (short|long)-term reference", the missing field is ignored and instead the next
        // available stored reference field of the chosen parity from the ordered list of frames
        // refFrameListX(Short|Long)Term is inserted into RefPicListX.
        ref_frame_list.retain(|h| {
            let p = h.pic.borrow();
            let skip = p.nonexisting || *p.reference() != reference_type;
            !skip
        });

        while let Some(position) = ref_frame_list.iter().position(|h| {
            let p = h.pic.borrow();
            let found = p.field == field;

            if found {
                field = field.opposite();
            }

            found
        }) {
            let pic = ref_frame_list.remove(position);
            ref_pic_list.push(pic);
        }

        ref_pic_list.append(ref_frame_list);
    }

    /// 8.2.4.2.1 Initialization process for the reference picture list for P
    /// and SP slices in frames
    fn build_ref_pic_list_p(&self) -> DpbPicRefList<T> {
        let mut ref_pic_list_p0: Vec<_> =
            self.short_term_refs_iter().filter(|h| !h.pic.borrow().is_second_field()).collect();

        Self::sort_pic_num_descending(&mut ref_pic_list_p0);

        let num_short_term_refs = ref_pic_list_p0.len();

        ref_pic_list_p0
            .extend(self.long_term_refs_iter().filter(|h| !h.pic.borrow().is_second_field()));
        Self::sort_long_term_pic_num_ascending(&mut ref_pic_list_p0[num_short_term_refs..]);

        #[cfg(debug_assertions)]
        Self::debug_ref_list_p(&ref_pic_list_p0, false);

        ref_pic_list_p0
    }

    /// 8.2.4.2.2 Initialization process for the reference picture list for P
    /// and SP slices in fields
    fn build_ref_field_pic_list_p(&self, cur_pic: &PictureData) -> DpbPicRefList<T> {
        let mut ref_pic_list_p0 = vec![];

        let mut ref_frame_list_0_short_term: Vec<_> = self.short_term_refs_iter().collect();
        Self::sort_frame_num_wrap_descending(&mut ref_frame_list_0_short_term);

        let mut ref_frame_list_long_term: Vec<_> = self.long_term_refs_iter().collect();
        Self::sort_long_term_pic_num_ascending(&mut ref_frame_list_long_term);

        // 8.2.4.2.5
        Self::init_ref_field_pic_list(
            cur_pic.field,
            Reference::ShortTerm,
            &mut ref_frame_list_0_short_term,
            &mut ref_pic_list_p0,
        );
        Self::init_ref_field_pic_list(
            cur_pic.field,
            Reference::LongTerm,
            &mut ref_frame_list_long_term,
            &mut ref_pic_list_p0,
        );

        #[cfg(debug_assertions)]
        Self::debug_ref_list_p(&ref_pic_list_p0, true);

        ref_pic_list_p0
    }

    // 8.2.4.2.3 Initialization process for reference picture lists for B slices
    // in frames
    fn build_ref_pic_list_b(&self, cur_pic: &PictureData) -> (DpbPicRefList<T>, DpbPicRefList<T>) {
        let mut short_term_refs: Vec<_> =
            self.short_term_refs_iter().filter(|h| !h.pic.borrow().is_second_field()).collect();

        // When pic_order_cnt_type is equal to 0, reference pictures that are
        // marked as "non-existing" as specified in clause 8.2.5.2 are not
        // included in either RefPicList0 or RefPicList1.
        if cur_pic.pic_order_cnt_type == 0 {
            short_term_refs.retain(|h| !h.pic.borrow().nonexisting);
        }

        let mut ref_pic_list_b0 = vec![];
        let mut ref_pic_list_b1 = vec![];
        let mut remaining = vec![];
        // b0 contains three inner lists of pictures, i.e. [[0] [1] [2]]
        // [0]: short term pictures with POC < current, sorted by descending POC.
        // [1]: short term pictures with POC > current, sorted by ascending POC.
        // [2]: long term pictures sorted by ascending long_term_pic_num
        for &handle in &short_term_refs {
            let pic = handle.pic.borrow();

            if pic.pic_order_cnt < cur_pic.pic_order_cnt {
                ref_pic_list_b0.push(handle);
            } else {
                remaining.push(handle);
            }
        }

        Self::sort_poc_descending(&mut ref_pic_list_b0);
        Self::sort_poc_ascending(&mut remaining);
        ref_pic_list_b0.append(&mut remaining);

        let mut long_term_refs: Vec<_> = self
            .long_term_refs_iter()
            .filter(|h| !h.pic.borrow().nonexisting)
            .filter(|h| !h.pic.borrow().is_second_field())
            .collect();
        Self::sort_long_term_pic_num_ascending(&mut long_term_refs);

        ref_pic_list_b0.extend(long_term_refs.clone());

        // b1 contains three inner lists of pictures, i.e. [[0] [1] [2]]
        // [0]: short term pictures with POC > current, sorted by ascending POC.
        // [1]: short term pictures with POC < current, sorted by descending POC.
        // [2]: long term pictures sorted by ascending long_term_pic_num
        for &handle in &short_term_refs {
            let pic = handle.pic.borrow();

            if pic.pic_order_cnt > cur_pic.pic_order_cnt {
                ref_pic_list_b1.push(handle);
            } else {
                remaining.push(handle);
            }
        }

        Self::sort_poc_ascending(&mut ref_pic_list_b1);
        Self::sort_poc_descending(&mut remaining);

        ref_pic_list_b1.extend(remaining);
        ref_pic_list_b1.extend(long_term_refs);

        // When the reference picture list RefPicList1 has more than one entry
        // and RefPicList1 is identical to the reference picture list
        // RefPicList0, the first two entries RefPicList1[0] and RefPicList1[1]
        // are switched.
        Self::swap_b1_if_needed(&ref_pic_list_b0, &mut ref_pic_list_b1);

        #[cfg(debug_assertions)]
        Self::debug_ref_list_b(&ref_pic_list_b0, "ref_pic_list_b0");
        #[cfg(debug_assertions)]
        Self::debug_ref_list_b(&ref_pic_list_b1, "ref_pic_list_b1");

        (ref_pic_list_b0, ref_pic_list_b1)
    }

    /// 8.2.4.2.4 Initialization process for reference picture lists for B
    /// slices in fields
    fn build_ref_field_pic_list_b(
        &self,
        cur_pic: &PictureData,
    ) -> (DpbPicRefList<T>, DpbPicRefList<T>) {
        let mut ref_pic_list_b0 = vec![];
        let mut ref_pic_list_b1 = vec![];
        let mut ref_frame_list_0_short_term = vec![];
        let mut ref_frame_list_1_short_term = vec![];

        let mut remaining = vec![];

        let mut short_term_refs: Vec<_> = self.short_term_refs_iter().collect();

        // When pic_order_cnt_type is equal to 0, reference pictures that are
        // marked as "non-existing" as specified in clause 8.2.5.2 are not
        // included in either RefPicList0 or RefPicList1.
        if cur_pic.pic_order_cnt_type == 0 {
            short_term_refs.retain(|h| !h.pic.borrow().nonexisting);
        }

        // refFrameList0ShortTerm is comprised of two inner lists, [[0] [1]]
        // [0]: short term pictures with POC <= current, sorted by descending POC
        // [1]: short term pictures with POC > current, sorted by ascending POC
        // NOTE 3 – When the current field follows in decoding order a coded
        // field fldPrev with which together it forms a complementary reference
        // field pair, fldPrev is included into the list refFrameList0ShortTerm
        // using PicOrderCnt( fldPrev ) and the ordering method described in the
        // previous sentence is applied.
        for &handle in &short_term_refs {
            let pic = handle.pic.borrow();

            if pic.pic_order_cnt <= cur_pic.pic_order_cnt {
                ref_frame_list_0_short_term.push(handle);
            } else {
                remaining.push(handle);
            }
        }

        Self::sort_poc_descending(&mut ref_frame_list_0_short_term);
        Self::sort_poc_ascending(&mut remaining);
        ref_frame_list_0_short_term.append(&mut remaining);

        // refFrameList1ShortTerm is comprised of two inner lists, [[0] [1]]
        // [0]: short term pictures with POC > current, sorted by ascending POC
        // [1]: short term pictures with POC <= current, sorted by descending POC
        // NOTE 4 – When the current field follows in decoding order a coded
        // field fldPrev with which together it forms a complementary reference
        // field pair, fldPrev is included into the list refFrameList1ShortTerm
        // using PicOrderCnt( fldPrev ) and the ordering method described in the
        // previous sentence is applied.

        for &handle in &short_term_refs {
            let pic = handle.pic.borrow();

            if pic.pic_order_cnt > cur_pic.pic_order_cnt {
                ref_frame_list_1_short_term.push(handle);
            } else {
                remaining.push(handle);
            }
        }

        Self::sort_poc_ascending(&mut ref_frame_list_1_short_term);
        Self::sort_poc_descending(&mut remaining);
        ref_frame_list_1_short_term.append(&mut remaining);

        // refFrameListLongTerm: long term pictures sorted by ascending
        // LongTermFrameIdx.
        // NOTE 5 – When the current picture is the second field of a
        // complementary field pair and the first field of the complementary
        // field pair is marked as "used for long-term reference", the first
        // field is included into the list refFrameListLongTerm. A reference
        // entry in which only one field is marked as "used for long-term
        // reference" is included into the list refFrameListLongTerm
        let mut ref_frame_list_long_term: Vec<_> =
            self.long_term_refs_iter().filter(|h| !h.pic.borrow().nonexisting).collect();

        Self::sort_long_term_frame_idx_ascending(&mut ref_frame_list_long_term);

        #[cfg(debug_assertions)]
        Self::debug_ref_list_b(&ref_frame_list_0_short_term, "ref_frame_list_0_short_term");
        #[cfg(debug_assertions)]
        Self::debug_ref_list_b(&ref_frame_list_1_short_term, "ref_frame_list_1_short_term");
        #[cfg(debug_assertions)]
        Self::debug_ref_list_b(&ref_frame_list_long_term, "ref_frame_list_long_term");

        // 8.2.4.2.5
        let field = cur_pic.field;
        Self::init_ref_field_pic_list(
            field,
            Reference::ShortTerm,
            &mut ref_frame_list_0_short_term,
            &mut ref_pic_list_b0,
        );
        Self::init_ref_field_pic_list(
            field,
            Reference::LongTerm,
            &mut ref_frame_list_long_term,
            &mut ref_pic_list_b0,
        );

        Self::init_ref_field_pic_list(
            field,
            Reference::ShortTerm,
            &mut ref_frame_list_1_short_term,
            &mut ref_pic_list_b1,
        );
        Self::init_ref_field_pic_list(
            field,
            Reference::LongTerm,
            &mut ref_frame_list_long_term,
            &mut ref_pic_list_b1,
        );

        // When the reference picture list RefPicList1 has more than one entry
        // and RefPicList1 is identical to the reference picture list
        // RefPicList0, the first two entries RefPicList1[0] and RefPicList1[1]
        // are switched.
        Self::swap_b1_if_needed(&ref_pic_list_b0, &mut ref_pic_list_b1);

        #[cfg(debug_assertions)]
        Self::debug_ref_list_b(&ref_pic_list_b0, "ref_pic_list_b0");
        #[cfg(debug_assertions)]
        Self::debug_ref_list_b(&ref_pic_list_b1, "ref_pic_list_b1");

        (ref_pic_list_b0, ref_pic_list_b1)
    }

    /// Returns the lists of reference pictures for `pic`.
    pub fn build_ref_pic_lists(&self, pic: &PictureData) -> ReferencePicLists {
        let num_refs = self.pictures().filter(|p| p.is_ref() && !p.nonexisting).count();

        // 8.2.4.2.1 ~ 8.2.4.2.4: When this process is invoked, there shall be
        // at least one reference frame or complementary reference field pair
        // that is currently marked as "used for reference" (i.e., as "used for
        // short-term reference" or "used for long-term reference") and is not
        // marked as "non-existing".
        if num_refs == 0 {
            return Default::default();
        }

        let (ref_pic_list_p0, (ref_pic_list_b0, ref_pic_list_b1)) =
            if matches!(pic.field, Field::Frame) {
                (self.build_ref_pic_list_p(), self.build_ref_pic_list_b(pic))
            } else {
                (self.build_ref_field_pic_list_p(pic), self.build_ref_field_pic_list_b(pic))
            };

        let dpb_start = self.entries.as_ptr();
        let refs_to_index = |refs: Vec<_>| {
            refs.into_iter()
                .map(|r| r as *const DpbEntry<T>)
                .map(|r| unsafe { r.offset_from(dpb_start) })
                .map(|i| i as usize)
                .collect()
        };

        ReferencePicLists {
            ref_pic_list_p0: refs_to_index(ref_pic_list_p0),
            ref_pic_list_b0: refs_to_index(ref_pic_list_b0),
            ref_pic_list_b1: refs_to_index(ref_pic_list_b1),
        }
    }
}

impl<T> Default for Dpb<T> {
    fn default() -> Self {
        // See https://github.com/rust-lang/rust/issues/26925 on why this can't
        // be derived.
        Self {
            entries: Default::default(),
            max_num_pics: Default::default(),
            max_num_reorder_frames: Default::default(),
            interlaced: Default::default(),
        }
    }
}

impl<T> std::fmt::Debug for Dpb<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pics = self.entries.iter().map(|h| &h.pic).enumerate().collect::<Vec<_>>();
        f.debug_struct("Dpb")
            .field("pictures", &pics)
            .field("max_num_pics", &self.max_num_pics)
            .field("interlaced", &self.interlaced)
            .finish()
    }
}
