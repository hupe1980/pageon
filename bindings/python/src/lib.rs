//! Python bindings of pageon (module `pageon._native`). The friendly API lives in
//! `python/pageon/__init__.py`; this module stays thin.

use pageon_agent::{Budget, Capture, Episode, Mode, Tokenizer, count_tokens, cut_head_tail};
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyDict;

create_exception!(
    _native,
    PageonError,
    PyException,
    "A pageon error; the message says what to do next."
);

fn budget(max_tokens: usize, tokenizer: &str) -> PyResult<Budget> {
    let tokenizer = Tokenizer::parse(tokenizer).ok_or_else(|| {
        PageonError::new_err(format!("unknown tokenizer {tokenizer:?}; use \"o200k\""))
    })?;
    Ok(Budget {
        tokenizer,
        max_tokens,
    })
}

/// A captured page state.
#[pyclass(name = "Capture", module = "pageon._native", frozen)]
struct PyCapture {
    inner: Capture,
}

#[pymethods]
impl PyCapture {
    /// Loads a capture file (`.capture.json.gz` or `.json`).
    #[staticmethod]
    fn load(path: &str) -> PyResult<Self> {
        Capture::load(path)
            .map(|inner| Self { inner })
            .map_err(|e| PageonError::new_err(e.to_string()))
    }

    /// Parses a capture from JSON text.
    #[staticmethod]
    fn from_json(text: &str) -> PyResult<Self> {
        Capture::from_json(text)
            .map(|inner| Self { inner })
            .map_err(|e| PageonError::new_err(e.to_string()))
    }

    /// URL of the top-level document.
    #[getter]
    fn url(&self) -> String {
        self.inner.url.clone()
    }

    /// Title of the top-level document.
    #[getter]
    fn title(&self) -> String {
        self.inner.title.clone()
    }

    /// Step index within the episode.
    #[getter]
    fn step(&self) -> u32 {
        self.inner.meta.step
    }

    /// Baseline outputs stored at capture time, keyed by format.
    #[getter]
    fn baselines(&self) -> std::collections::BTreeMap<String, String> {
        self.inner.baselines.clone()
    }

    /// Centre of a node's visible box in viewport CSS pixels (for coordinate clicks), searching
    /// the given frame or, if `None`, every frame.
    #[pyo3(signature = (backend_node_id, frame_id = None))]
    fn center(&self, backend_node_id: i64, frame_id: Option<&str>) -> Option<(f64, f64)> {
        let page = pageon_agent::page::Page::build(&self.inner);
        page.frames
            .iter()
            .filter(|f| frame_id.is_none_or(|id| f.frame_id == id))
            .find_map(|f| {
                let i = *f.by_backend.get(&backend_node_id)?;
                let b = f.dom[i].layout.as_ref()?.bounds;
                let v = b.intersect(&page.viewport)?;
                Some((
                    v.x + v.w / 2.0 - page.viewport.x,
                    v.y + v.h / 2.0 - page.viewport.y,
                ))
            })
    }

    /// The `bid` attribute of a node, if BrowserGym set one.
    fn bid_of(&self, frame_id: &str, backend_node_id: i64) -> Option<String> {
        let page = pageon_agent::page::Page::build(&self.inner);
        let f = page.frames.iter().find(|f| f.frame_id == frame_id)?;
        let i = *f.by_backend.get(&backend_node_id)?;
        f.dom[i]
            .attr("bid")
            .or_else(|| f.dom[i].attr("browsergym_id"))
            .map(str::to_string)
    }
}

/// The stateless `ax` view of one capture.
#[pyfunction]
#[pyo3(signature = (capture, budget = 4000, tokenizer = "o200k"))]
fn ax(capture: &PyCapture, budget: usize, tokenizer: &str) -> PyResult<String> {
    Ok(pageon_agent::ax(
        &capture.inner,
        self::budget(budget, tokenizer)?,
    ))
}

/// o200k token count (shared by every format).
#[pyfunction]
#[pyo3(name = "count_tokens")]
fn py_count_tokens(text: &str) -> usize {
    count_tokens(text)
}

/// Cuts a baseline's text to the budget: head and tail lines plus one omission line.
#[pyfunction]
#[pyo3(name = "cut_head_tail", signature = (text, budget, tokenizer = "o200k"))]
fn py_cut_head_tail(text: &str, budget: usize, tokenizer: &str) -> PyResult<String> {
    Ok(cut_head_tail(text, self::budget(budget, tokenizer)?))
}

/// One task played once (native part; use `pageon.Episode`).
#[pyclass(name = "_Episode", module = "pageon._native")]
struct PyEpisode {
    inner: Episode,
}

#[pymethods]
impl PyEpisode {
    #[new]
    #[pyo3(signature = (budget = 4000, tokenizer = "o200k", mode = "full"))]
    fn new(budget: usize, tokenizer: &str, mode: &str) -> PyResult<Self> {
        let mode = match mode {
            "full" => Mode::Full,
            "diff" => Mode::Diff,
            m => {
                return Err(PageonError::new_err(format!(
                    "unknown mode {m:?}; use \"full\" or \"diff\""
                )));
            }
        };
        Ok(Self {
            inner: Episode::new(self::budget(budget, tokenizer)?, mode),
        })
    }

    #[getter]
    fn version(&self) -> u64 {
        self.inner.version()
    }

    fn observe(&mut self, capture: &PyCapture) -> String {
        self.inner.observe(&capture.inner)
    }

    #[pyo3(signature = (verb, reference, error = None))]
    fn note_action(&mut self, verb: &str, reference: &str, error: Option<&str>) {
        self.inner.note_action(verb, reference, error);
    }

    /// Returns a dict: `{"ok": True, backend_node_id, frame_id, bid, relocated}` or
    /// `{"ok": False, kind, message, since_step?, candidates?, covered_by?}`.
    fn resolve_raw<'py>(
        &self,
        py: Python<'py>,
        reference: &str,
        capture: &PyCapture,
    ) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        match self.inner.resolve(reference, &capture.inner) {
            Ok(t) => {
                d.set_item("ok", true)?;
                d.set_item("backend_node_id", t.backend_node_id)?;
                d.set_item("frame_id", t.frame_id)?;
                d.set_item("bid", t.bid)?;
                d.set_item("relocated", t.relocated)?;
            }
            Err(e) => {
                d.set_item("ok", false)?;
                d.set_item("kind", e.kind())?;
                d.set_item("message", e.to_string())?;
                match e {
                    pageon_agent::RefError::Stale { since_step } => {
                        d.set_item("since_step", since_step)?
                    }
                    pageon_agent::RefError::Ambiguous { candidates } => {
                        d.set_item("candidates", candidates)?
                    }
                    pageon_agent::RefError::Covered { covered_by } => {
                        d.set_item("covered_by", covered_by)?
                    }
                    _ => {}
                }
            }
        }
        Ok(d)
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("PageonError", m.py().get_type::<PageonError>())?;
    m.add_class::<PyCapture>()?;
    m.add_class::<PyEpisode>()?;
    m.add_function(wrap_pyfunction!(ax, m)?)?;
    m.add_function(wrap_pyfunction!(py_count_tokens, m)?)?;
    m.add_function(wrap_pyfunction!(py_cut_head_tail, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
