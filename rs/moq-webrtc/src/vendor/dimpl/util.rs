use arrayvec::ArrayVec;
use nom::error::{ErrorKind, ParseError, make_error};
use nom::{Err, IResult, Input, Parser};

/// A combinator that parses items using the provided parser but only collects
/// items that pass a filter predicate. Allows zero matches.
#[inline(always)]
pub fn many0<I, O, E, F, P, const N: usize>(
    mut parser: F,
    predicate: P,
) -> impl FnMut(I) -> IResult<I, ArrayVec<O, N>, E>
where
    I: Clone + Input,
    F: Parser<I, Output = O, Error = E>,
    P: Fn(&O) -> bool,
    E: ParseError<I>,
{
    move |mut i: I| {
        let mut acc = ArrayVec::new();

        loop {
            let len = i.input_len();
            if len == 0 {
                break;
            }

            match parser.parse(i.clone()) {
                Err(Err::Error(_)) => break,
                Err(e) => return Err(e),
                Ok((i1, o)) => {
                    // infinite loop check: the parser must always consume
                    if i1.input_len() == len {
                        return Err(Err::Error(E::from_error_kind(i, ErrorKind::Many0)));
                    }

                    i = i1;
                    // Only collect items that pass the filter
                    if predicate(&o) && acc.try_push(o).is_err() {
                        return Err(Err::Error(E::from_error_kind(i, ErrorKind::Many0)));
                    }
                }
            }
        }

        Ok((i, acc))
    }
}

/// A combinator that parses items using the provided parser but only collects
/// items that pass a filter predicate. Requires at least one item to pass the filter.
#[inline(always)]
pub fn many1<I, O, E, F, P, const N: usize>(
    mut parser: F,
    predicate: P,
) -> impl FnMut(I) -> IResult<I, ArrayVec<O, N>, E>
where
    I: Clone + Input,
    F: Parser<I, Output = O, Error = E>,
    P: Fn(&O) -> bool,
    E: ParseError<I>,
{
    move |mut i: I| {
        let mut acc = ArrayVec::new();
        let original_input = i.clone();

        loop {
            let len = i.input_len();
            if len == 0 {
                break;
            }

            match parser.parse(i.clone()) {
                Err(Err::Error(_)) => break,
                Err(e) => return Err(e),
                Ok((i1, o)) => {
                    // infinite loop check: the parser must always consume
                    if i1.input_len() == len {
                        return Err(Err::Error(E::from_error_kind(i, ErrorKind::Many1)));
                    }

                    i = i1;
                    // Only collect items that pass the filter
                    if predicate(&o) && acc.try_push(o).is_err() {
                        return Err(Err::Error(E::from_error_kind(i, ErrorKind::Many1)));
                    }
                }
            }
        }

        // Require at least one item to pass the filter
        if acc.is_empty() {
            Err(Err::Error(E::from_error_kind(
                original_input,
                ErrorKind::Many1,
            )))
        } else {
            Ok((i, acc))
        }
    }
}

pub fn be_u48<I, E: ParseError<I>>(input: I) -> IResult<I, u64, E>
where
    I: Input<Item = u8>,
{
    let bound: usize = 6;

    if input.input_len() < bound {
        Err(Err::Error(make_error(input, ErrorKind::Eof)))
    } else {
        let mut res = 0u64;

        for byte in input.iter_elements().take(bound) {
            res = (res << 8) + byte as u64;
        }

        Ok((input.take_from(bound), res))
    }
}
