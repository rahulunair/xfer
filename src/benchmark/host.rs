use std::collections::BTreeSet;
use std::fs;

use crate::output::HostInfo;

pub(crate) fn discover_host() -> HostInfo {
    let fallback_logical_cpus = std::thread::available_parallelism().map_or(0, usize::from);
    fs::read_to_string("/proc/cpuinfo").map_or_else(
        |_| HostInfo {
            cpu_model: "unknown".to_owned(),
            logical_cpus: fallback_logical_cpus,
            physical_cores: None,
            sockets: None,
        },
        |cpuinfo| parse_cpuinfo(&cpuinfo, fallback_logical_cpus),
    )
}

fn parse_cpuinfo(cpuinfo: &str, fallback_logical_cpus: usize) -> HostInfo {
    let mut cpu_model = None;
    let mut logical_cpus = 0;
    let mut sockets = BTreeSet::new();
    let mut cores = BTreeSet::new();

    for record in cpuinfo.split("\n\n") {
        let mut has_processor = false;
        let mut physical_id = None;
        let mut core_id = None;

        for line in record.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "processor" => has_processor = true,
                "model name" | "hardware" if cpu_model.is_none() && !value.is_empty() => {
                    cpu_model = Some(value.to_owned());
                }
                "physical id" => physical_id = Some(value.to_owned()),
                "core id" => core_id = Some(value.to_owned()),
                _ => {}
            }
        }

        if has_processor {
            logical_cpus += 1;
        }
        if let Some(physical_id) = physical_id {
            sockets.insert(physical_id.clone());
            if let Some(core_id) = core_id {
                cores.insert((physical_id, core_id));
            }
        }
    }

    HostInfo {
        cpu_model: cpu_model.unwrap_or_else(|| "unknown".to_owned()),
        logical_cpus: if logical_cpus == 0 {
            fallback_logical_cpus
        } else {
            logical_cpus
        },
        physical_cores: (!cores.is_empty()).then_some(cores.len()),
        sockets: (!sockets.is_empty()).then_some(sockets.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_model_socket_core_and_thread_counts() {
        let cpuinfo = "\
processor : 0
model name : Example CPU
physical id : 0
core id : 0

processor : 1
model name : Example CPU
physical id : 0
core id : 0

processor : 2
model name : Example CPU
physical id : 1
core id : 0
";

        assert_eq!(
            parse_cpuinfo(cpuinfo, 1),
            HostInfo {
                cpu_model: "Example CPU".to_owned(),
                logical_cpus: 3,
                physical_cores: Some(2),
                sockets: Some(2),
            }
        );
    }

    #[test]
    fn falls_back_when_topology_fields_are_missing() {
        assert_eq!(
            parse_cpuinfo("Hardware : Example SoC\n", 8),
            HostInfo {
                cpu_model: "Example SoC".to_owned(),
                logical_cpus: 8,
                physical_cores: None,
                sockets: None,
            }
        );
    }
}
