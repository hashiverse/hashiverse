pub fn anyhow_to_napi(err: anyhow::Error) -> napi::Error {
    napi::Error::from_reason(format!("{:#}", err))
}

pub fn join_err_to_napi(err: tokio::task::JoinError) -> napi::Error {
    napi::Error::from_reason(format!("background task join error: {err}"))
}

/// Run an async block on the client's dedicated tokio runtime from a blocking
/// thread, mirroring the Python crate's `py_try!` macro.
///
/// Why this exists: napi-rs's `#[napi] async fn` requires the returned future
/// to be `Send` for all lifetimes — but several `hashiverse_lib::client`
/// methods produce futures that hold non-`Send`-for-all-lifetimes borrows
/// across awaits (lock guards over `Arc<dyn …>` trait objects, etc). The
/// Python crate avoids this by running everything inside `runtime.block_on(...)`
/// from a thread that has released the GIL; we do the same trick here: spawn a
/// blocking thread (so the outer future stays trivially `Send`) and `block_on`
/// the inner work on our dedicated runtime. No code change needed in
/// `hashiverse_lib`.
#[macro_export]
macro_rules! napi_try {
    ($runtime:expr, { $($body:tt)* }) => {{
        let runtime = $runtime;
        ::tokio::task::spawn_blocking(move || {
            runtime.block_on(async move {
                anyhow::Ok({ $($body)* })
            })
        })
        .await
        .map_err($crate::napi_err::join_err_to_napi)?
        .map_err($crate::napi_err::anyhow_to_napi)
    }};
}
