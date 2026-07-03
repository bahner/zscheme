/// Scheme module — thin re-export of ma-zscheme.
///
/// All evaluator logic lives in the `ma-zscheme` crate.
/// This module provides path-compatible re-exports so the rest of zscheme
/// can continue using `crate::scheme::*`.
pub use ma_zscheme::{eval_source, init_session_env, SchemeErr, SchemeVal};
