macro_rules! assert_vec_contains{
    (@match $vector:expr, $pattern:pat if $cond:expr => $success:expr) => {
        let mut has_value = false;
        for x in $vector.iter() {
            match x {
                $pattern if $cond => {has_value = true; $success},
                _ => ()
            };
        }

        if !has_value {
            panic!("Vector had {} elements but none matched '{} if {}'", $vector.len(), stringify!($pattern), stringify!($cond))
        }
    };

    // Entry points
    ($vector:expr, $pattern:pat if $cond:expr) => {
        assert_vec_contains!(@match $vector, $pattern if $cond => ());
    };

    ($vector:expr, $pattern:pat => $success:expr) => {
        assert_vec_contains!(@match $vector, $pattern if true => $success);
    };

    ($vector:expr, $pattern:pat if $cond:expr => $success:expr) => {
        assert_vec_contains!(@match $vector, $pattern if $cond => $success);
    };

    ($vector:expr, $pattern:pat) => {
        assert_vec_contains!(@match $vector, $pattern if true => ());
    };
}
