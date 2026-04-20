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