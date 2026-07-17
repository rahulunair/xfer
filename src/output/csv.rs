use super::model::{
    BenchCase, BenchReport, CaseOutcome, Endpoint, LinkInfo, Operation, QueueStreamInfo,
};

pub const BENCH_CSV_HEADER: &str = "status,transfer_class,operation,peer_access,src_device,dst_device,bytes,size,allocation,queue_ordinal,queue_copy,queue_compute,timing_mode,warmup_ms,samples,negotiated_pcie_link,negotiated_pcie_theoretical_gb_s,median_gb_s,median_ci_lower_gb_s,median_ci_upper_gb_s,confidence_level,bootstrap_resamples,mad_gb_s,p5_gb_s,p95_gb_s,outliers_mild,outliers_severe,skip_reason,benchmark_mode,stream_count,queue_streams,second_phase_stream_count,second_phase_queue_streams,logical_payload_bytes,submitted_copy_bytes,submission_policy,staging_barrier";

pub fn render_bench_csv(report: &BenchReport) -> String {
    let mut lines = Vec::with_capacity(report.cases.len() + 1);
    lines.push(BENCH_CSV_HEADER.to_owned());

    for case in &report.cases {
        lines.push(render_case_csv(case));
    }

    finish_lines(lines)
}

pub fn render_case_csv(case: &BenchCase) -> String {
    let mut fields = Vec::new();
    let (status, summary, time_summary, skip_reason) = match &case.outcome {
        CaseOutcome::Measured {
            summary,
            time_summary,
            ..
        } => ("measured", Some(summary), Some(time_summary), ""),
        CaseOutcome::Skipped { reason } => ("skipped", None, None, reason.as_str()),
    };

    fields.push(status.to_owned());
    fields.push(case.transfer_class.to_string());
    fields.push(render_operation_field(&case.operation).to_owned());
    fields.push(render_peer_access_field(&case.operation).to_owned());
    fields.push(render_endpoint_field(&case.source));
    fields.push(render_endpoint_field(&case.destination));
    fields.push(case.byte_count.to_string());
    fields.push(super::text::format_bytes(case.byte_count));
    fields.push(case.allocation.to_string());
    fields.push(
        case.selected_group
            .as_ref()
            .map_or_else(String::new, |group| group.ordinal.to_string()),
    );
    fields.push(
        case.selected_group
            .as_ref()
            .map_or_else(String::new, |group| group.flags.copy.to_string()),
    );
    fields.push(
        case.selected_group
            .as_ref()
            .map_or_else(String::new, |group| group.flags.compute.to_string()),
    );
    fields.push(case.timing.to_string());
    fields.push(case.warmup.as_millis().to_string());
    fields.push(case.requested_samples.to_string());
    fields.push(render_link_field(&case.pcie_link));
    fields.push(render_link_theoretical_field(&case.pcie_link));

    if let Some(summary) = summary {
        fields.push(format_float(summary.median));
        fields.push(format_float(summary.median_confidence.lower_bound));
        fields.push(format_float(summary.median_confidence.upper_bound));
        fields.push(format_float(summary.median_confidence.confidence_level));
        fields.push(summary.median_confidence.resamples.to_string());
        fields.push(format_float(summary.mad));
        fields.push(format_float(summary.p5));
        fields.push(format_float(summary.p95));
        let time_summary = time_summary.expect("measured cases have duration statistics");
        fields.push(time_summary.outliers.counts.mild.to_string());
        fields.push(time_summary.outliers.counts.severe.to_string());
    } else {
        for _ in 0..10 {
            fields.push(String::new());
        }
    }

    fields.push(skip_reason.to_owned());
    fields.push(case.mode.to_string());
    fields.push(case.streams.len().to_string());
    fields.push(render_streams(&case.streams));
    fields.push(case.second_phase_streams.len().to_string());
    fields.push(render_streams(&case.second_phase_streams));
    fields.push(case.byte_count.to_string());
    fields.push(submitted_copy_bytes(case).to_string());
    fields.push(submission_policy(case).to_owned());
    fields.push(staging_barrier(case).to_owned());

    fields
        .into_iter()
        .map(|field| csv_escape(&field))
        .collect::<Vec<_>>()
        .join(",")
}

fn render_streams(streams: &[QueueStreamInfo]) -> String {
    streams
        .iter()
        .map(|stream| format!("g{}:q{}", stream.group_ordinal, stream.queue_index))
        .collect::<Vec<_>>()
        .join(";")
}

fn submitted_copy_bytes(case: &BenchCase) -> u64 {
    if matches!(case.operation, Operation::ExplicitStaged) {
        case.byte_count.saturating_mul(2)
    } else {
        case.byte_count
    }
}

fn submission_policy(case: &BenchCase) -> &'static str {
    match case.mode {
        crate::cli::BenchMode::Single => "submit-one-sync-one",
        crate::cli::BenchMode::Saturation => "prepare-all-submit-all-sync-all",
    }
}

fn staging_barrier(case: &BenchCase) -> &'static str {
    if matches!(case.operation, Operation::ExplicitStaged) {
        "after-d2h-all"
    } else {
        ""
    }
}

pub fn csv_escape(value: &str) -> String {
    if value
        .bytes()
        .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn render_operation_field(operation: &Operation) -> &'static str {
    match operation {
        Operation::HostToDevice => "h2d-pinned",
        Operation::DeviceToHost => "d2h-pinned",
        Operation::SameDevice => "same-device",
        Operation::Direct { .. } => "direct",
        Operation::ExplicitStaged => "explicit-staged",
    }
}

fn render_peer_access_field(operation: &Operation) -> &str {
    match operation {
        Operation::Direct { peer_access } => peer_access.as_field(),
        Operation::HostToDevice
        | Operation::DeviceToHost
        | Operation::SameDevice
        | Operation::ExplicitStaged => "",
    }
}

fn render_endpoint_field(endpoint: &Endpoint) -> String {
    endpoint.to_string()
}

fn render_link_field(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            generation, width, ..
        } => format!("Gen{generation}x{width}"),
        LinkInfo::Unknown { reason } => format!("unknown:{reason}"),
    }
}

fn render_link_theoretical_field(link: &LinkInfo) -> String {
    match link {
        LinkInfo::Known {
            theoretical_gb_s, ..
        } => format_float(*theoretical_gb_s),
        LinkInfo::Unknown { .. } => String::new(),
    }
}

fn format_float(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
    } else {
        String::new()
    }
}

fn finish_lines(lines: Vec<String>) -> String {
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::cli::{TimingMode, TransferClass};
    use crate::output::{
        AllocationKind, CaseOutcome, Endpoint, LinkInfo, Operation, QueueFlags, QueueGroupInfo,
    };
    use crate::stats;

    use super::*;

    fn measured_case() -> BenchCase {
        let samples = vec![49.8, 50.7, 51.2, 51.6, 51.9];
        let summary = stats::summarize(&samples).expect("summary");

        BenchCase {
            mode: crate::cli::BenchMode::Single,
            selected_group: Some(QueueGroupInfo {
                ordinal: 1,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
                queue_count: 1,
            }),
            streams: vec![crate::output::QueueStreamInfo {
                group_ordinal: 1,
                queue_index: 0,
                flags: QueueFlags {
                    copy: true,
                    compute: false,
                },
            }],
            second_phase_streams: Vec::new(),
            transfer_class: TransferClass::D2H,
            operation: Operation::DeviceToHost,
            source: Endpoint::Device(0),
            destination: Endpoint::Host,
            byte_count: 256 * 1024 * 1024,
            allocation: AllocationKind::PinnedHost,
            timing: TimingMode::WallClock,
            warmup: Duration::from_secs(1),
            requested_samples: 5,
            pcie_link: LinkInfo::Known {
                generation: 5,
                width: 16,
                theoretical_gb_s: 63.015_384,
            },
            outcome: CaseOutcome::Measured {
                time_summary: Box::new(summary),
                summary,
                samples_gb_s: samples,
            },
        }
    }

    #[test]
    fn csv_header_is_stable() {
        let columns = BENCH_CSV_HEADER.split(',').collect::<Vec<_>>();
        let legacy = "status,transfer_class,operation,peer_access,src_device,dst_device,bytes,size,allocation,queue_ordinal,queue_copy,queue_compute,timing_mode,warmup_ms,samples,negotiated_pcie_link,negotiated_pcie_theoretical_gb_s,median_gb_s,median_ci_lower_gb_s,median_ci_upper_gb_s,confidence_level,bootstrap_resamples,mad_gb_s,p5_gb_s,p95_gb_s,outliers_mild,outliers_severe,skip_reason"
            .split(',')
            .collect::<Vec<_>>();

        assert_eq!(columns.len(), 37);
        assert_eq!(&columns[..legacy.len()], legacy);
        assert_eq!(columns[0], "status");
        assert_eq!(columns[1], "transfer_class");
        assert_eq!(columns[9], "queue_ordinal");
        assert_eq!(columns[27], "skip_reason");
        assert_eq!(columns[28], "benchmark_mode");
        assert_eq!(columns[36], "staging_barrier");
    }

    #[test]
    fn csv_escaping_handles_commas_quotes_and_newlines() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_escape("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn measured_csv_has_exact_stable_columns_and_no_ansi() {
        let mut case = measured_case();
        case.pcie_link = LinkInfo::Unknown {
            reason: "bad, \"quoted\" path".to_owned(),
        };
        let report = BenchReport { cases: vec![case] };

        let csv = render_bench_csv(&report);
        let lines = csv.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], BENCH_CSV_HEADER);
        assert_eq!(
            split_csv_record(lines[0]).len(),
            split_csv_record(lines[1]).len()
        );
        assert_eq!(split_csv_record(lines[0]).len(), 37);
        assert!(lines[1].contains("\"unknown:bad, \"\"quoted\"\" path\""));
        assert!(!csv.contains("\u{1b}["));
    }

    #[test]
    fn skipped_direct_csv_is_machine_readable() {
        let report = BenchReport {
            cases: vec![BenchCase {
                mode: crate::cli::BenchMode::Single,
                selected_group: Some(QueueGroupInfo {
                    ordinal: 0,
                    flags: QueueFlags {
                        copy: true,
                        compute: true,
                    },
                    queue_count: 1,
                }),
                streams: vec![crate::output::QueueStreamInfo {
                    group_ordinal: 0,
                    queue_index: 0,
                    flags: QueueFlags {
                        copy: true,
                        compute: true,
                    },
                }],
                second_phase_streams: Vec::new(),
                transfer_class: TransferClass::D2DDirect,
                operation: Operation::Direct {
                    peer_access: crate::output::PeerAccess::No,
                },
                source: Endpoint::Device(0),
                destination: Endpoint::Device(1),
                byte_count: 1024,
                allocation: AllocationKind::Device,
                timing: TimingMode::DeviceTimestamps,
                warmup: Duration::from_millis(250),
                requested_samples: 10,
                pcie_link: LinkInfo::Unknown {
                    reason: "missing sysfs mapping".to_owned(),
                },
                outcome: CaseOutcome::Skipped {
                    reason: "peer access unsupported".to_owned(),
                },
            }],
        };

        let csv = render_bench_csv(&report);
        assert!(csv.contains("skipped,d2d-direct,direct,no,dev0,dev1"));
        assert!(csv.contains("peer access unsupported"));
    }

    #[test]
    fn saturation_csv_reports_streams_and_logical_payload() {
        let mut case = measured_case();
        case.mode = crate::cli::BenchMode::Saturation;
        case.selected_group = None;
        case.streams.push(crate::output::QueueStreamInfo {
            group_ordinal: 2,
            queue_index: 0,
            flags: QueueFlags {
                copy: true,
                compute: true,
            },
        });

        let fields = split_csv_record(&render_case_csv(&case));

        assert_eq!(fields[28], "saturation");
        assert_eq!(fields[29], "2");
        assert_eq!(fields[30], "g1:q0;g2:q0");
        assert_eq!(fields[33], case.byte_count.to_string());
        assert_eq!(fields[34], case.byte_count.to_string());
        assert_eq!(fields[35], "prepare-all-submit-all-sync-all");
        assert_eq!(fields[36], "");
    }

    #[test]
    fn staged_saturation_csv_reports_both_phases_and_copy_traffic() {
        let mut case = measured_case();
        case.mode = crate::cli::BenchMode::Saturation;
        case.operation = Operation::ExplicitStaged;
        case.second_phase_streams = vec![crate::output::QueueStreamInfo {
            group_ordinal: 3,
            queue_index: 1,
            flags: QueueFlags {
                copy: true,
                compute: false,
            },
        }];

        let fields = split_csv_record(&render_case_csv(&case));

        assert_eq!(fields[31], "1");
        assert_eq!(fields[32], "g3:q1");
        assert_eq!(fields[34], case.byte_count.saturating_mul(2).to_string());
        assert_eq!(fields[36], "after-d2h-all");
    }

    fn split_csv_record(line: &str) -> Vec<String> {
        let mut fields = Vec::new();
        let mut field = String::new();
        let mut chars = line.chars().peekable();
        let mut quoted = false;

        while let Some(ch) = chars.next() {
            match ch {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = !quoted,
                ',' if !quoted => {
                    fields.push(field);
                    field = String::new();
                }
                _ => field.push(ch),
            }
        }

        fields.push(field);
        fields
    }
}
