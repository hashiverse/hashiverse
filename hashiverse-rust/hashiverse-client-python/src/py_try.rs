use pyo3::PyErr;
use crate::HashiverseError;

pub fn anyhow_to_py(err: anyhow::Error) -> PyErr {
    HashiverseError::new_err(format!("{:#}", err))
}

/// Macro modelled after `wasm_try!` — wraps an async block, converts
/// `anyhow::Error` to `HashiverseError`.
///
/// NB: Also runs async code on the client's dedicated tokio runtime, releasing the GIL
/// so other Python threads can proceed while the Rust future executes.
#[macro_export]
macro_rules! py_try {
    ($py:expr, $runtime:expr, { $($tt:tt)* }) => {{
        $py.allow_threads(|| {
            $runtime.block_on(async {
                anyhow::Ok({ $($tt)* })
            })
        })
        .map_err($crate::py_try::anyhow_to_py)
    }};
}
