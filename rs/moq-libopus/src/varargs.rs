//! A bit more type-safe variadic function APIs. Not really ABI-compatible with C, just a compat layer.

pub enum VarArg<'a> {
    I32(i32),
    I32Out(&'a mut i32),
    U32Out(&'a mut u32),
    F32Ptr(*mut f32),
    U8Ptr(*mut u8),
    CustomModeOut(&'a mut *const crate::OpusCustomMode),
    OpusDecoderOut(&'a mut *mut crate::OpusDecoder),
    OpusEncoderOut(&'a mut *mut crate::OpusEncoder),
    AnalysisInfoOut(&'a mut crate::src::analysis::AnalysisInfo),
    SilkInfoOut(&'a mut crate::celt::celt_encoder::SILKInfo),
}

pub trait FromVarArg<'a> {
    fn from_vararg(arg: VarArg<'a>) -> Self;
}

pub trait IntoVarArg<'a> {
    fn into_vararg(self) -> VarArg<'a>;
}

macro_rules! impl_from_vararg {
    ($t:ty, $v:ident) => {
        impl<'a> FromVarArg<'a> for $t {
            fn from_vararg(arg: VarArg<'a>) -> Self {
                match arg {
                    VarArg::$v(v) => v,
                    _ => panic!("invalid vararg type"),
                }
            }
        }
    };
}
macro_rules! impl_into_vararg {
    ($t:ty, $v:ident) => {
        impl<'a> IntoVarArg<'a> for $t {
            fn into_vararg(self) -> VarArg<'a> {
                VarArg::$v(self)
            }
        }
    };
}
macro_rules! impl_vararg {
    ($t:ty, $v:ident) => {
        impl_from_vararg!($t, $v);
        impl_into_vararg!($t, $v);
    };
}

impl_vararg!(i32, I32);
impl_vararg!(&'a mut i32, I32Out);
impl_vararg!(&'a mut u32, U32Out);
impl_vararg!(*mut f32, F32Ptr);
impl_vararg!(*mut u8, U8Ptr);
impl_vararg!(&'a mut *const crate::OpusCustomMode, CustomModeOut);
impl_vararg!(&'a mut *mut crate::OpusDecoder, OpusDecoderOut);
impl_vararg!(&'a mut *mut crate::OpusEncoder, OpusEncoderOut);
impl_vararg!(&'a mut crate::src::analysis::AnalysisInfo, AnalysisInfoOut);
impl_vararg!(&'a mut crate::celt::celt_encoder::SILKInfo, SilkInfoOut);

pub struct VarArgs<'a>(pub Vec<VarArg<'a>>);

impl<'a> VarArgs<'a> {
    pub fn new(mut varargs: Vec<VarArg<'a>>) -> Self {
        // reverse them, because we pop them off the end, but want to have order "from left to right"
        varargs.reverse();
        Self(varargs)
    }

    pub fn arg<T: FromVarArg<'a>>(&mut self) -> T {
        T::from_vararg(
            self.0
                .pop()
                .expect("Attempt to pop from empty varargs (are the function arguments correct?)"),
        )
    }
}

#[macro_export]
macro_rules! varargs {
    ($($arg:expr),*) => {
        $crate::varargs::VarArgs::new(
            vec![
                $(
                    $crate::varargs::IntoVarArg::into_vararg($arg)
                ),*
            ]
        )
    };
}
