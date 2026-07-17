//! Output model, rendering, and streaming reporting.

mod csv;
mod model;
mod terminal;
mod text;

use crate::cli::OutputFormat;

pub use self::csv::{BENCH_CSV_HEADER, csv_escape, render_bench_csv, render_case_csv};
pub use self::model::{
    AllocationKind, BenchCase, BenchReport, CaseOutcome, ColorMode, DeviceInfo, Endpoint, HostInfo,
    LinkInfo, ListReport, Operation, PeerAccess, PeerAccessInfo, PeerRoute, QueueFlags,
    QueueGroupInfo, QueueStreamInfo, SystemInfo, TextOptions,
};
pub use self::terminal::{InteractiveReporter, LiveReporter, StatusMode};
pub use self::text::{render_bench_summary, render_bench_text, render_case_text, render_list};

pub fn render_bench(report: &BenchReport, format: OutputFormat, text: &TextOptions) -> String {
    match format {
        OutputFormat::Text => render_bench_text(report, text),
        OutputFormat::Csv => render_bench_csv(report),
    }
}

#[cfg(test)]
pub(crate) fn test_system(device_count: u32) -> SystemInfo {
    SystemInfo {
        host: HostInfo {
            cpu_model: "Test CPU".to_owned(),
            logical_cpus: 16,
            physical_cores: Some(8),
            sockets: Some(1),
        },
        devices: (0..device_count)
            .map(|index| DeviceInfo {
                index,
                name: "Test GPU".to_owned(),
                pci_address: Some(format!("0000:{:02x}:00.0", index + 1)),
                pcie_link: LinkInfo::Known {
                    generation: 5,
                    width: 16,
                    theoretical_gb_s: 63.015_384,
                },
                queue_groups: vec![
                    QueueGroupInfo {
                        ordinal: 0,
                        flags: QueueFlags {
                            copy: true,
                            compute: true,
                        },
                        queue_count: 1,
                    },
                    QueueGroupInfo {
                        ordinal: 1,
                        flags: QueueFlags {
                            copy: true,
                            compute: false,
                        },
                        queue_count: 1,
                    },
                ],
            })
            .collect(),
    }
}
