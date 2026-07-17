//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

macro_rules! expect_arguments_exact {
    (
        $args:expr,
        $expected_arguments:expr,
        $func:ident
    ) => {{
        let args = if let Some(args) = $args {
            if args.len() != $expected_arguments {
                crate::bail_parse_error!(
                    "{} function called with not exactly {} arguments",
                    $func.to_string(),
                    $expected_arguments,
                );
            }
            args
        } else {
            crate::bail_parse_error!("{} function with no arguments", $func.to_string());
        };

        args
    }};
}

macro_rules! expect_arguments_max {
    (
        $args:expr,
        $expected_arguments:expr,
        $func:ident
    ) => {{
        let args = if let Some(args) = $args {
            if args.len() > $expected_arguments {
                crate::bail_parse_error!(
                    "{} function called with more than {} arguments",
                    $func.to_string(),
                    $expected_arguments,
                );
            }
            args
        } else {
            crate::bail_parse_error!("{} function with no arguments", $func.to_string());
        };

        args
    }};
}

macro_rules! expect_arguments_min {
    (
        $args:expr,
        $expected_arguments:expr,
        $func:ident
    ) => {{
        let args = if let Some(args) = $args {
            if args.len() < $expected_arguments {
                crate::bail_parse_error!(
                    "{} function with less than {} arguments",
                    $func.to_string(),
                    $expected_arguments
                );
            }
            args
        } else {
            crate::bail_parse_error!("{} function with no arguments", $func.to_string());
        };
        args
    }};
}

#[allow(unused_macros)]
macro_rules! expect_arguments_even {
    (
        $args:expr,
        $func:ident
    ) => {{
        let args = $args.as_deref().unwrap_or_default();
        if args.len() % 2 != 0 {
            crate::bail_parse_error!(
                "{} function requires an even number of arguments",
                $func.to_string()
            );
        };
        // The only function right now that requires an even number is `json_object` and it allows
        // to have no arguments, so thats why in this macro we do not bail with the `function with no arguments` error
        args
    }};
}
