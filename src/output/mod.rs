//! Output model, rendering, and streaming reporting.

mod csv;
mod model;
mod terminal;
mod text;

use crate::cli::OutputFormat;

pub use self::csv::{BENCH_CSV_HEADER, csv_escape, render_bench_csv, render_case_csv};
pub use self::model::{
    AllocationKind, BenchCase, BenchReport, CaseOutcome, ColorMode, DeviceInfo, Endpoint, LinkInfo,
    ListReport, Operation, PeerAccess, PeerAccessInfo, QueueFlags, QueueGroupInfo, TextOptions,
};
pub use self::terminal::{LiveReporter, StatusMode};
pub use self::text::{render_bench_text, render_case_text, render_list};

pub fn render_bench(report: &BenchReport, format: OutputFormat, text: &TextOptions) -> String {
    match format {
        OutputFormat::Text => render_bench_text(report, text),
        OutputFormat::Csv => render_bench_csv(report),
    }
}
