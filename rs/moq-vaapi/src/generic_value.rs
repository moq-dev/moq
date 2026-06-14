// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use thiserror::Error;

use crate::bindings;

/// A wrapper over `VAGenericValue` giving us safe access to the underlying union members.
#[derive(Debug)]
pub enum GenericValue {
    /// A wrapper over VAGenericValueTypeInteger
    Integer(i32),
    /// A wrapper over VAGenericValueTypeFloat
    Float(f32),
    /// A wrapper over VAGenericValueTypePointer
    Pointer(*mut std::os::raw::c_void),
    /// A wrapper over VAGenericValueTypeFunc
    Func(bindings::VAGenericFunc),
}

#[derive(Debug, Error)]
pub enum GenericValueError {
    #[error("unexpected VAGenericValueType {0}")]
    UnexpectedType(u32),
}

impl TryFrom<bindings::VAGenericValue> for GenericValue {
    type Error = GenericValueError;

    fn try_from(value: bindings::VAGenericValue) -> Result<Self, Self::Error> {
        // Safe because we check the type before accessing the union.
        match value.type_ {
            // Safe because we check the type before accessing the union.
            bindings::VAGenericValueType::VAGenericValueTypeInteger => {
                Ok(Self::Integer(unsafe { value.value.i }))
            }
            bindings::VAGenericValueType::VAGenericValueTypeFloat => {
                Ok(Self::Float(unsafe { value.value.f }))
            }
            bindings::VAGenericValueType::VAGenericValueTypePointer => {
                Ok(Self::Pointer(unsafe { value.value.p }))
            }
            bindings::VAGenericValueType::VAGenericValueTypeFunc => {
                Ok(Self::Func(unsafe { value.value.fn_ }))
            }
            _ => Err(GenericValueError::UnexpectedType(value.type_)),
        }
    }
}
