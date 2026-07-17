#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::if_same_then_else,
    clippy::manual_div_ceil,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::trim_split_whitespace
)]

pub mod benchmark;
mod bootstrap;
pub mod cli;
pub mod histogram;
pub mod level_zero;
pub mod output;
pub mod pcie;
pub mod stats;
