//! This module provides a series of asserts that interact nicely with raising anyhow errors inside try blocks.

#[macro_export]
macro_rules! anyhow_assert_eq {
    ($expected:expr, $actual:expr) => {{
        let expected_val = &($expected);
        let actual_val = &($actual);
        if expected_val != actual_val {
            let msg = format!("expected={:?} actual={:?}", expected_val, actual_val);
            let perform_assert_fail = |msg: String| -> anyhow::Result<()> {
                anyhow::bail!(msg);
            };
            perform_assert_fail(msg)?;
        }
    }};

    ($expected:expr, $actual:expr,) => {
        anyhow_assert_eq!($expected, $actual)
    };

    ($expected:expr, $actual:expr, $($arg:tt)*) => {{
        let expected_val = &($expected);
        let actual_val = &($actual);
        if expected_val != actual_val {
            let msg = format!("{}: expected={:?} actual={:?}", format_args!($($arg)*), expected_val, actual_val);
            let perform_assert_fail = |msg: String| -> anyhow::Result<()> {
                anyhow::bail!(msg);
            };
            perform_assert_fail(msg)?;
        }
    }};
}

#[macro_export]
macro_rules! anyhow_assert {
    ($cond:expr) => {{
        if !($cond) {
            let msg = format!("assertion failed: {}", stringify!($cond));
            let perform_assert_fail = |msg: String| -> anyhow::Result<()> {
                anyhow::bail!(msg);
            };
            perform_assert_fail(msg)?;
        }
    }};

    ($cond:expr,) => {
        anyhow_assert!($cond)
    };

    ($cond:expr, $($arg:tt)*) => {{
        if !($cond) {
            let msg = format!("{}", format_args!($($arg)*));
            let perform_assert_fail = |msg: String| -> anyhow::Result<()> {
                anyhow::bail!(msg);
            };
            perform_assert_fail(msg)?;
        }
    }};
}

#[macro_export]
macro_rules! anyhow_assert_ge {
    ($lhs:expr, $rhs:expr) => {{
        let lhs_val = &($lhs);
        let rhs_val = &($rhs);
        if lhs_val < rhs_val {
            let msg = format!("expected {:?} >= {:?}", lhs_val, rhs_val);
            let perform_assert_fail = |msg: String| -> anyhow::Result<()> {
                anyhow::bail!(msg);
            };
            perform_assert_fail(msg)?;
        }
    }};

    ($lhs:expr, $rhs:expr,) => {
        anyhow_assert_ge!($lhs, $rhs)
    };

    ($lhs:expr, $rhs:expr, $($arg:tt)*) => {{
     let lhs_val = &($lhs);
        let rhs_val = &($rhs);
        if lhs_val < rhs_val {
            let msg = format!("{}: expected {:?} >= {:?}", format_args!($($arg)*), lhs_val, rhs_val);
            let perform_assert_fail = |msg: String| -> anyhow::Result<()> {
                anyhow::bail!(msg);
            };
            perform_assert_fail(msg)?;
        }
    }};
}