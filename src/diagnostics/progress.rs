use std::io;

use crate::benchmark::{BenchEvent, NoopReport, Report};

use super::model::{CounterPhase, EvidenceFailure};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticPhase {
    Discovery,
    StagedCalibration,
    DirectMemory,
    DirectPeerWrite,
    DirectPeerRead,
    OptionalUpi,
    Acs,
    Synthesis,
}

impl DiagnosticPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::StagedCalibration => "staged calibration",
            Self::DirectMemory => "direct memory",
            Self::DirectPeerWrite => "direct peer-write",
            Self::DirectPeerRead => "direct peer-read",
            Self::OptionalUpi => "optional UPI",
            Self::Acs => "ACS",
            Self::Synthesis => "synthesis",
        }
    }

    pub(crate) fn from_counter_phase(phase: CounterPhase) -> Self {
        match phase {
            CounterPhase::ExplicitStagedMemory => Self::StagedCalibration,
            CounterPhase::DirectMemory => Self::DirectMemory,
            CounterPhase::DirectPeerWrite => Self::DirectPeerWrite,
            CounterPhase::DirectPeerRead => Self::DirectPeerRead,
            CounterPhase::DirectUpi => Self::OptionalUpi,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticPhaseStatus {
    Complete,
    Unavailable,
}

pub const P2P_DIAGNOSTIC_PHASES: [DiagnosticPhase; 8] = [
    DiagnosticPhase::Discovery,
    DiagnosticPhase::StagedCalibration,
    DiagnosticPhase::DirectMemory,
    DiagnosticPhase::DirectPeerWrite,
    DiagnosticPhase::DirectPeerRead,
    DiagnosticPhase::OptionalUpi,
    DiagnosticPhase::Acs,
    DiagnosticPhase::Synthesis,
];

pub fn phase_position(phase: DiagnosticPhase) -> (usize, usize) {
    let index = P2P_DIAGNOSTIC_PHASES
        .iter()
        .position(|candidate| *candidate == phase)
        .expect("diagnostic phase is present in phase order")
        + 1;
    (index, P2P_DIAGNOSTIC_PHASES.len())
}

pub trait P2pDiagnosticProgress: Report {
    fn phase_started(&mut self, _phase: DiagnosticPhase) -> io::Result<()> {
        Ok(())
    }

    fn phase_finished(
        &mut self,
        _phase: DiagnosticPhase,
        _status: DiagnosticPhaseStatus,
        _reason: Option<&EvidenceFailure>,
    ) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopP2pDiagnosticProgress {
    benchmark: NoopReport,
}

impl Report for NoopP2pDiagnosticProgress {
    fn event(&mut self, event: BenchEvent<'_>) -> io::Result<()> {
        self.benchmark.event(event)
    }
}

impl P2pDiagnosticProgress for NoopP2pDiagnosticProgress {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_phase_order_and_labels_are_stable() {
        let labels = P2P_DIAGNOSTIC_PHASES
            .iter()
            .map(|phase| phase.label())
            .collect::<Vec<_>>();

        assert_eq!(
            labels,
            [
                "discovery",
                "staged calibration",
                "direct memory",
                "direct peer-write",
                "direct peer-read",
                "optional UPI",
                "ACS",
                "synthesis",
            ]
        );
        assert_eq!(
            phase_position(DiagnosticPhase::DirectPeerRead),
            (5, P2P_DIAGNOSTIC_PHASES.len())
        );
    }

    #[test]
    fn counter_phases_map_to_operator_facing_diagnostic_labels() {
        assert_eq!(
            DiagnosticPhase::from_counter_phase(CounterPhase::ExplicitStagedMemory),
            DiagnosticPhase::StagedCalibration
        );
        assert_eq!(
            DiagnosticPhase::from_counter_phase(CounterPhase::DirectUpi),
            DiagnosticPhase::OptionalUpi
        );
    }
}
