use wasm_bindgen::JsValue;
use anyhow::anyhow;


pub trait JsResultExt<T> {
    fn with_js_context<F, S>(self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> S,
        S: Into<String>;
}

impl<T> JsResultExt<T> for std::result::Result<T, JsValue> {
    fn with_js_context<F, S>(self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> S,
        S: Into<String>
    {
        self.map_err(|e| anyhow!("{}: {:?}", f().into(), e))
    }
}

impl<T> JsResultExt<T> for std::result::Result<T, indexed_db_futures::error::Error> {
    fn with_js_context<F, S>(self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> S,
        S: Into<String>
    {
        // IdbError usually implements Display, so we can use {} instead of {:?}
        self.map_err(|e| anyhow::anyhow!("{}: {}", f().into(), e))
    }
}