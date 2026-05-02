#[macro_use]
pub mod py_try;
pub mod hashiverse_client_python;

use pyo3::prelude::*;
use hashiverse_client_python::*;

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

pyo3::create_exception!(hashiverse_client, HashiverseError, pyo3::exceptions::PyException);

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Install the Ring crypto provider for rustls — required for TLS connections.
    let _ = rustls::crypto::ring::default_provider().install_default();

    m.add("HashiverseError", m.py().get_type::<HashiverseError>())?;
    m.add_class::<HashiverseClientPython>()?;
    m.add_class::<Post>()?;
    m.add_class::<Bio>()?;
    m.add_class::<UrlPreview>()?;
    m.add_class::<TrendingHashtag>()?;
    m.add_class::<TimelineResponse>()?;

    // Register the HashiverseClientPython under its Python-facing name
    m.add("HashiverseClient", m.py().get_type::<HashiverseClientPython>())?;

    Ok(())
}
