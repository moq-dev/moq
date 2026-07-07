#[allow(unused)]
macro_rules! assert_vec_match{
    (@empty $vector:expr) => {
        if $vector.len() > 0 {
            panic!("Expected an empty vector, but the vector contained {} elements", $vector.len());
        }
    };

    (@match $idx:expr, $vector:expr, $pattern:pat if $cond:expr => $success:expr) => {
        if $vector.len() <= $idx {
            panic!("Could not match on index {} as the vector only has {} values", $idx, $vector.len());
        }

        match $vector[$idx] {
            $pattern if $cond => $success,
            _ => panic!("Match failed on index {}: {:?} vs {} ", $idx, $vector[$idx], stringify!($pattern))
        };
    };

    (@step $idx:expr, $vector:expr,) => {
        if $vector.len() > $idx {
            panic!("Vector contained {} elements but only {} was expected!", $vector.len(), $idx);
        }
    };

    (@step $idx:expr, $vector:expr) => {
        if $vector.len() > $idx {
            panic!("Vector contained {} elements but only {} was expected!", $vector.len(), $idx);
        }
    };

    // pattern if condition
    (@step $idx:expr, $vector:expr, $pattern:pat if $cond:expr, $($rest:tt)*) => {
        assert_vec_match!(@match $idx, $vector, $pattern if $cond => ());
        assert_vec_match!(@step $idx + 1usize, $vector, $($rest)*);
    };

    (@step $idx:expr, $vector:expr, $pattern:pat if $cond:expr) => {
        assert_vec_match!(@match $idx, $vector, $pattern if $cond => $success);
    };

    // pattern if condition => success
    (@step $idx:expr, $vector:expr, $pattern:pat if $cond:expr => $success:expr, $($rest:tt)*) => {
        assert_vec_match!(@match $idx, $vector, $pattern if $cond => $success);
        assert_vec_match!(@step $idx + 1usize, $vector, $($rest)*);
    };

    (@step $idx:expr, $vector:expr, $pattern:pat if $cond:expr => $success:expr) => {
        assert_vec_match!(@match $idx, $vector, $pattern if $cond => $success);
    };

    // pattern => success
    (@step $idx:expr, $vector:expr, $pattern:pat => $success:expr, $($rest:tt)*) => {
        assert_vec_match!(@match $idx, $vector, $pattern if true => $success);
        assert_vec_match!(@step $idx + 1usize, $vector, $($rest)*);
    };

    (@step $idx:expr, $vector:expr, $pattern:pat => $success:expr) => {
        assert_vec_match!(@match $idx, $vector, $pattern if true => $success);
    };

    // pattern only
    (@step $idx:expr, $vector:expr, $pattern:pat, $($rest:tt)*) => {
        assert_vec_match!(@match $idx, $vector, $pattern if true => ());
        assert_vec_match!(@step $idx + 1usize, $vector, $($rest)*);
    };

    (@step $idx:expr, $vector:expr, $pattern:pat) => {
        assert_vec_match!(@match $idx, $vector, $pattern if true => ());
    };

    // Entry points
    ($vector:expr, $pattern:pat if $cond:expr, $($rest:tt)*) => {
        assert_vec_match!(@step 0usize, $vector, $pattern if $cond => (), $($rest)*);
    };

    ($vector:expr, $pattern:pat if $cond:expr) => {
        assert_vec_match!(@step 0usize, $vector, $pattern if $cond => ());
    };

    ($vector:expr, $pattern:pat => $success:expr, $($rest:tt)*) => {
        assert_vec_match!(@step 0usize, $vector, $pattern if true => $success, $($rest)*);
    };

    ($vector:expr, $pattern:pat => $success:expr) => {
        assert_vec_match!(@step 0usize, $vector, $pattern if true => $success);
    };

    ($vector:expr, $pattern:pat if $cond:expr => $success:expr, $($rest:tt)*) => {
        assert_vec_match!(@step 0usize, $vector, $pattern if $cond => $success, $($rest)*);
    };

    ($vector:expr, $pattern:pat if $cond:expr => $success:expr) => {
        assert_vec_match!(@step 0usize, $vector, $pattern if $cond => $success);
    };

    ($vector:expr, $pattern:pat, $($rest:tt)*) => {
        assert_vec_match!(@step 0usize, $vector, $pattern if true => (), $($rest)*);
    };

    ($vector:expr, $pattern:pat) => {
        assert_vec_match!(@step 0usize, $vector, $pattern if true => ());
    };

    // Empty macro after vector
    ($vector:expr) => {
        assert_vec_match!(@empty $vector);
    };

    ($vector:expr,) => {
        assert_vec_match!(@empty $vector);
    };
}
