use wasm_bindgen::prelude::*;
pub fn anyhow_to_js(err: anyhow::Error) -> JsValue {
    js_sys::Error::new(&format!("{:#}", err)).into()
}

#[macro_export]
macro_rules! wasm_try {
    ({ $($tt:tt)* }) => {{
        (async move {
            let result: anyhow::Result<_> = try {
                $($tt)*
            };
            result
        })
        .await
        .map_err($crate::wasm_try::anyhow_to_js)
    }};
}
/// Convert a value to `tsify::Ts<T>` for returning across the wasm boundary.
///
/// Replaces the deprecated `#[tsify(into_wasm_abi)]`: serialization now happens inside our
/// function as an ordinary `Result`, so a failure unwinds normally instead of leaking the wasm heap
/// (<https://github.com/madonoharu/tsify/issues/65>).  `tsify::Error` wraps a `serde_wasm_bindgen::Error`
/// holding a `JsValue` (not `Send + Sync`), hence the string conversion rather than a plain `?`.
pub fn to_ts<T: tsify::Tsify + serde::Serialize>(value: &T) -> anyhow::Result<tsify::Ts<T>> {
    tsify::Tsify::into_ts(value).map_err(|e| anyhow::anyhow!("{}", e))
}

/// Element-wise [`to_ts`]: tsify supports `Vec<Ts<T>>` but not `Ts<Vec<T>>`.
pub fn to_ts_vec<T: tsify::Tsify + serde::Serialize>(values: &[T]) -> anyhow::Result<Vec<tsify::Ts<T>>> {
    values.iter().map(to_ts).collect()
}

/// [`wasm_try!`] whose block evaluates to a `Tsify` value, returned as `Ts<T>`.
#[macro_export]
macro_rules! wasm_try_ts {
    ({ $($tt:tt)* }) => {{
        $crate::wasm_try!({
            let value = { $($tt)* };
            $crate::wasm_try::to_ts(&value)?
        })
    }};
}

/// [`wasm_try!`] whose block evaluates to a `Vec` of `Tsify` values, returned as `Vec<Ts<T>>`.
#[macro_export]
macro_rules! wasm_try_ts_vec {
    ({ $($tt:tt)* }) => {{
        $crate::wasm_try!({
            let values: Vec<_> = { $($tt)* };
            $crate::wasm_try::to_ts_vec(&values)?
        })
    }};
}
