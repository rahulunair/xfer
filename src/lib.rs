#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

pub mod benchmark;
mod bootstrap;
pub mod cli;
pub mod diagnostics;
pub mod evidence;
pub mod histogram;
pub mod level_zero;
pub mod output;
pub mod pcie;
pub mod stats;
