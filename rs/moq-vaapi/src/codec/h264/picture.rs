// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;
use std::rc::Weak;

use log::debug;

use crate::codec::h264::parser::MaxLongTermFrameIdx;
use crate::codec::h264::parser::RefPicMarking;
use crate::codec::h264::parser::Slice;
use crate::codec::h264::parser::SliceType;
use crate::codec::h264::parser::Sps;
use crate::Resolution;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Field {
    #[default]
    Frame,
    Top,
    Bottom,
}

impl Field {
    /// Returns the field of opposite parity.
    pub fn opposite(&self) -> Self {
        match *self {
            Field::Frame => Field::Frame,
            Field::Top => Field::Bottom,
            Field::Bottom => Field::Top,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Reference {
    #[default]
    None,
    ShortTerm,
    LongTerm,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum IsIdr {
    #[default]
    No,
    Yes {
        idr_pic_id: u16,
    },
}

/// The rank of a field, i.e. whether it is the first or second one to be parsed from the stream.
/// This is unrelated to the `Field` type, as the first field can be either `Top` or `Bottom`.
#[derive(Default, Debug)]
pub enum FieldRank {
    /// Frame has a single field.
    #[default]
    Single,
    /// Frame is interlaced, and this is the first field (with a reference to the second one).
    First(Weak<RefCell<PictureData>>),
    /// Frame is interlaced, and this is the second field (with a reference to the first one).
    Second(Rc<RefCell<PictureData>>),
}

#[derive(Default)]
pub struct PictureData {
    pub pic_order_cnt_type: u8,
    pub top_field_order_cnt: i32,
    pub bottom_field_order_cnt: i32,
    pub pic_order_cnt: i32,
    pub pic_order_cnt_msb: i32,
    pub pic_order_cnt_lsb: i32,
    pub delta_pic_order_cnt_bottom: i32,
    pub delta_pic_order_cnt0: i32,
    pub delta_pic_order_cnt1: i32,

    pub pic_num: i32,
    pub long_term_pic_num: u32,
    pub frame_num: u32,
    pub frame_num_offset: u32,
    pub frame_num_wrap: i32,
    pub long_term_frame_idx: u32,

    pub coded_resolution: Resolution,
    pub display_resolution: Resolution,

    pub type_: SliceType,
    pub nal_ref_idc: u8,
    pub is_idr: IsIdr,
    reference: Reference,
    pub ref_pic_list_modification_flag_l0: i32,
    pub abs_diff_pic_num_minus1: i32,

    // Does memory management op 5 needs to be executed after this
    // picture has finished decoding?
    pub has_mmco_5: bool,

    // Created by the decoding process for gaps in frame_num.
    // Not for decode or output.
    pub nonexisting: bool,

    pub field: Field,

    // Values from slice_hdr to be used during reference marking and
    // memory management after finishing this picture.
    pub ref_pic_marking: RefPicMarking,

    field_rank: FieldRank,

    pub timestamp: u64,
}

/// A `PictureData` within a `Rc<RefCell>` which field rank is guaranteed to be correct.
///
/// The field rank of `PictureData` is only final after both fields have been constructed - namely,
/// the first field can only point to the second one after the latter is available as a Rc. Methods
/// [`PictureData::into_rc`] and [`PictureData::split_frame`] take care of this, and is this only
/// producer of this type, ensuring all instances are correct.
#[derive(Default, Debug, Clone)]
pub struct RcPictureData {
    pic: Rc<RefCell<PictureData>>,
}

impl Deref for RcPictureData {
    type Target = Rc<RefCell<PictureData>>;

    fn deref(&self) -> &Self::Target {
        &self.pic
    }
}

impl PictureData {
    pub fn new_non_existing(frame_num: u32, timestamp: u64) -> Self {
        PictureData {
            frame_num,
            nonexisting: true,
            nal_ref_idc: 1,
            field: Field::Frame,
            pic_num: frame_num as i32,
            reference: Reference::ShortTerm,
            timestamp,
            ..Default::default()
        }
    }

    /// Create a new picture from a `slice`, `sps`, and `timestamp`.
    ///
    /// `first_field` is set if this picture is the second field of a frame.
    pub fn new_from_slice(
        slice: &Slice,
        sps: &Sps,
        timestamp: u64,
        first_field: Option<&RcPictureData>,
    ) -> Self {
        let hdr = &slice.header;
        let nalu_hdr = &slice.nalu.header;

        let is_idr = if nalu_hdr.idr_pic_flag {
            IsIdr::Yes { idr_pic_id: hdr.idr_pic_id }
        } else {
            IsIdr::No
        };

        let field = if hdr.field_pic_flag {
            if hdr.bottom_field_flag {
                Field::Bottom
            } else {
                Field::Top
            }
        } else {
            Field::Frame
        };

        let reference = if nalu_hdr.ref_idc != 0 { Reference::ShortTerm } else { Reference::None };

        let pic_num = if !hdr.field_pic_flag { hdr.frame_num } else { 2 * hdr.frame_num + 1 };

        let (
            pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom,
            delta_pic_order_cnt0,
            delta_pic_order_cnt1,
        ) = match sps.pic_order_cnt_type {
            0 => (
                hdr.pic_order_cnt_lsb,
                hdr.delta_pic_order_cnt_bottom,
                Default::default(),
                Default::default(),
            ),
            1 => (
                Default::default(),
                Default::default(),
                hdr.delta_pic_order_cnt[0],
                hdr.delta_pic_order_cnt[1],
            ),
            _ => (Default::default(), Default::default(), Default::default(), Default::default()),
        };

        let coded_resolution = Resolution::from((sps.width(), sps.height()));

        let visible_rect = sps.visible_rectangle();

        let display_resolution = Resolution {
            width: visible_rect.max.x - visible_rect.min.x,
            height: visible_rect.max.y - visible_rect.min.y,
        };

        let mut pic = PictureData {
            pic_order_cnt_type: sps.pic_order_cnt_type,
            pic_order_cnt_lsb: i32::from(pic_order_cnt_lsb),
            delta_pic_order_cnt_bottom,
            delta_pic_order_cnt0,
            delta_pic_order_cnt1,
            pic_num: i32::from(pic_num),
            frame_num: u32::from(hdr.frame_num),
            nal_ref_idc: nalu_hdr.ref_idc,
            is_idr,
            reference,
            field,
            ref_pic_marking: hdr.dec_ref_pic_marking.clone(),
            coded_resolution,
            display_resolution,
            timestamp,
            ..Default::default()
        };

        if let Some(first_field) = first_field {
            pic.set_first_field_to(first_field);
        }

        pic
    }

    /// Whether the current picture is a reference, either ShortTerm or LongTerm.
    pub fn is_ref(&self) -> bool {
        !matches!(self.reference, Reference::None)
    }

    /// Whether this picture is a second field.
    pub fn is_second_field(&self) -> bool {
        matches!(self.field_rank, FieldRank::Second(..))
    }

    /// Returns the field rank of this picture, including a reference to its other field.
    pub fn field_rank(&self) -> &FieldRank {
        &self.field_rank
    }

    /// Returns a reference to the picture's Reference
    pub fn reference(&self) -> &Reference {
        &self.reference
    }

    /// Mark the picture as a reference picture.
    pub fn set_reference(&mut self, reference: Reference, apply_to_other_field: bool) {
        log::debug!("Set reference of {:#?} to {:?}", self, reference);
        self.reference = reference;

        if apply_to_other_field {
            if let Some(other_field) = self.other_field() {
                log::debug!(
                    "other_field: Set reference of {:#?} to {:?}",
                    &other_field.borrow(),
                    reference
                );
                other_field.borrow_mut().reference = reference;
            }
        }
    }

    /// Get a reference to the picture's other field, if there is any
    /// and its reference is still valid.
    pub fn other_field(&self) -> Option<Rc<RefCell<PictureData>>> {
        match &self.field_rank {
            FieldRank::Single => None,
            FieldRank::First(other_field) => other_field.upgrade(),
            FieldRank::Second(other_field) => Some(other_field.clone()),
        }
    }

    /// Set this picture's second field.
    fn set_second_field_to(&mut self, other_field: &Rc<RefCell<Self>>) {
        self.field_rank = FieldRank::First(Rc::downgrade(other_field));
    }

    /// Whether the current picture is the second field of a complementary ref pair.
    pub fn is_second_field_of_complementary_ref_pair(&self) -> bool {
        self.is_ref()
            && matches!(self.field_rank(), FieldRank::Second(first_field) if first_field.borrow().is_ref())
    }

    /// Set this picture's first field.
    fn set_first_field_to(&mut self, other_field: &Rc<RefCell<Self>>) {
        self.field_rank = FieldRank::Second(other_field.clone());
    }

    pub fn pic_num_f(&self, max_pic_num: i32) -> i32 {
        if !matches!(self.reference(), Reference::LongTerm) {
            self.pic_num
        } else {
            max_pic_num
        }
    }

    pub fn long_term_pic_num_f(&self, max_long_term_frame_idx: MaxLongTermFrameIdx) -> u32 {
        if matches!(self.reference(), Reference::LongTerm) {
            self.long_term_pic_num
        } else {
            2 * max_long_term_frame_idx.to_value_plus1()
        }
    }

    /// Consume this picture and return a Rc'd version.
    ///
    /// If the picture was a second field, adjust the field of the first field to point to this
    /// one.
    pub fn into_rc(self) -> RcPictureData {
        let self_rc = Rc::new(RefCell::new(self));

        if let FieldRank::Second(first_field) = self_rc.borrow().field_rank() {
            first_field.borrow_mut().set_second_field_to(&self_rc);
        }

        RcPictureData { pic: self_rc }
    }

    /// Split a frame into two complementary fields that reference one another.
    pub fn split_frame(mut self) -> (RcPictureData, RcPictureData) {
        assert!(matches!(self.field, Field::Frame));
        assert!(matches!(self.field_rank, FieldRank::Single));

        debug!(
            "Splitting picture (frame_num, POC) ({:?}, {:?})",
            self.frame_num, self.pic_order_cnt
        );

        let second_pic_order_cnt = if self.top_field_order_cnt < self.bottom_field_order_cnt {
            self.field = Field::Top;
            self.pic_order_cnt = self.top_field_order_cnt;

            self.bottom_field_order_cnt
        } else {
            self.field = Field::Bottom;
            self.pic_order_cnt = self.bottom_field_order_cnt;

            self.top_field_order_cnt
        };

        let second_field = PictureData {
            top_field_order_cnt: self.top_field_order_cnt,
            bottom_field_order_cnt: self.bottom_field_order_cnt,
            frame_num: self.frame_num,
            reference: self.reference,
            nonexisting: self.nonexisting,
            pic_order_cnt: second_pic_order_cnt,
            field: self.field.opposite(),
            ..Default::default()
        };

        debug!(
            "Split into picture (frame_num, POC) ({:?}, {:?}), field: {:?}",
            self.frame_num, self.pic_order_cnt, self.field
        );
        debug!(
            "Split into picture (frame_num, POC) ({:?}, {:?}), field {:?}",
            second_field.frame_num, second_field.pic_order_cnt, second_field.field
        );

        let first_field = Rc::new(RefCell::new(self));
        let second_field = Rc::new(RefCell::new(second_field));

        first_field.borrow_mut().set_second_field_to(&second_field);
        second_field.borrow_mut().set_first_field_to(&first_field);

        (RcPictureData { pic: first_field }, RcPictureData { pic: second_field })
    }
}

impl std::fmt::Debug for PictureData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PictureData")
            .field("pic_order_cnt_type", &self.pic_order_cnt_type)
            .field("top_field_order_cnt", &self.top_field_order_cnt)
            .field("bottom_field_order_cnt", &self.bottom_field_order_cnt)
            .field("pic_order_cnt", &self.pic_order_cnt)
            .field("pic_order_cnt_msb", &self.pic_order_cnt_msb)
            .field("pic_order_cnt_lsb", &self.pic_order_cnt_lsb)
            .field("delta_pic_order_cnt_bottom", &self.delta_pic_order_cnt_bottom)
            .field("delta_pic_order_cnt0", &self.delta_pic_order_cnt0)
            .field("delta_pic_order_cnt1", &self.delta_pic_order_cnt1)
            .field("pic_num", &self.pic_num)
            .field("long_term_pic_num", &self.long_term_pic_num)
            .field("frame_num", &self.frame_num)
            .field("frame_num_offset", &self.frame_num_offset)
            .field("frame_num_wrap", &self.frame_num_wrap)
            .field("long_term_frame_idx", &self.long_term_frame_idx)
            .field("coded_resolution", &self.coded_resolution)
            .field("display_resolution", &self.display_resolution)
            .field("type_", &self.type_)
            .field("nal_ref_idc", &self.nal_ref_idc)
            .field("is_idr", &self.is_idr)
            .field("reference", &self.reference)
            .field("ref_pic_list_modification_flag_l0", &self.ref_pic_list_modification_flag_l0)
            .field("abs_diff_pic_num_minus1", &self.abs_diff_pic_num_minus1)
            .field("has_mmco_5", &self.has_mmco_5)
            .field("nonexisting", &self.nonexisting)
            .field("field", &self.field)
            .field("ref_pic_marking", &self.ref_pic_marking)
            .field("field_rank", &self.field_rank)
            .finish()
    }
}
