use std::time::Duration;

use crate::cli::{BenchMode, TransferClass};

pub(crate) use super::error::CaseExecutionError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SampleContext {
    pub(crate) transfer_class: TransferClass,
    pub(crate) byte_count: u64,
    pub(crate) zero_based_sample_index: u32,
    pub(crate) bench_mode: BenchMode,
}

pub(crate) trait MeasurementObserver {
    /// Implementations must call `operation` exactly once and return that result.
    /// Observer evidence/setup failures are recorded by the implementation, not
    /// converted into benchmark execution errors.
    fn observe(
        &mut self,
        context: &SampleContext,
        operation: &mut dyn FnMut() -> std::result::Result<Duration, CaseExecutionError>,
    ) -> std::result::Result<Duration, CaseExecutionError>;
}

pub(crate) struct NoopMeasurementObserver;

impl MeasurementObserver for NoopMeasurementObserver {
    fn observe(
        &mut self,
        _context: &SampleContext,
        operation: &mut dyn FnMut() -> std::result::Result<Duration, CaseExecutionError>,
    ) -> std::result::Result<Duration, CaseExecutionError> {
        operation()
    }
}

pub(crate) fn observe_sample(
    observer: &mut dyn MeasurementObserver,
    context: &SampleContext,
    mut operation: impl FnMut() -> std::result::Result<Duration, CaseExecutionError>,
) -> std::result::Result<Duration, CaseExecutionError> {
    let mut called = false;
    let mut guarded = || {
        assert!(!called, "measurement operation called more than once");
        called = true;
        operation()
    };
    let result = observer.observe(context, &mut guarded);
    assert!(called, "measurement observer did not run operation");
    result
}

pub(crate) fn sample_context(
    transfer_class: TransferClass,
    byte_count: u64,
    zero_based_sample_index: u32,
    bench_mode: BenchMode,
) -> SampleContext {
    SampleContext {
        transfer_class,
        byte_count,
        zero_based_sample_index,
        bench_mode,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use crate::cli::{BenchMode, TransferClass};

    use super::*;

    struct RecordingObserver<'log> {
        log: &'log RefCell<Vec<String>>,
        contexts: &'log RefCell<Vec<SampleContext>>,
    }

    impl MeasurementObserver for RecordingObserver<'_> {
        fn observe(
            &mut self,
            context: &SampleContext,
            operation: &mut dyn FnMut() -> std::result::Result<Duration, CaseExecutionError>,
        ) -> std::result::Result<Duration, CaseExecutionError> {
            self.contexts.borrow_mut().push(*context);
            self.log.borrow_mut().push(format!(
                "observe-start:{}:{}",
                context.transfer_class, context.zero_based_sample_index
            ));
            let result = operation();
            self.log.borrow_mut().push(format!(
                "observe-end:{}:{}",
                context.transfer_class, context.zero_based_sample_index
            ));
            result
        }
    }

    fn recording_observer<'log>(
        log: &'log RefCell<Vec<String>>,
        contexts: &'log RefCell<Vec<SampleContext>>,
    ) -> RecordingObserver<'log> {
        RecordingObserver { log, contexts }
    }

    #[test]
    fn noop_observer_calls_operation_once() {
        let mut observer = NoopMeasurementObserver;
        let calls = Cell::new(0);
        let context = sample_context(TransferClass::D2DDirect, 128, 3, BenchMode::Single);

        let elapsed = observe_sample(&mut observer, &context, || {
            calls.set(calls.get() + 1);
            Ok(Duration::from_nanos(7))
        })
        .expect("sample succeeds");

        assert_eq!(elapsed, Duration::from_nanos(7));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn synthetic_direct_lifecycle_observes_only_measured_samples_after_setup() {
        let log = RefCell::new(Vec::new());
        let contexts = RefCell::new(Vec::new());
        let mut observer = recording_observer(&log, &contexts);

        log.borrow_mut().push("warmup-prepare".to_owned());
        log.borrow_mut().push("warmup-operation".to_owned());

        for sample_index in 0..2 {
            log.borrow_mut().push(format!("clear:{sample_index}"));
            log.borrow_mut().push(format!("prepare:{sample_index}"));
            let context = sample_context(
                TransferClass::D2DDirect,
                4096,
                sample_index,
                BenchMode::Single,
            );
            observe_sample(&mut observer, &context, || {
                log.borrow_mut().push(format!("submit-sync:{sample_index}"));
                Ok(Duration::from_nanos(u64::from(sample_index + 1)))
            })
            .expect("sample succeeds");
            log.borrow_mut().push(format!("verify:{sample_index}"));
        }

        assert_eq!(
            log.into_inner(),
            [
                "warmup-prepare",
                "warmup-operation",
                "clear:0",
                "prepare:0",
                "observe-start:d2d-direct:0",
                "submit-sync:0",
                "observe-end:d2d-direct:0",
                "verify:0",
                "clear:1",
                "prepare:1",
                "observe-start:d2d-direct:1",
                "submit-sync:1",
                "observe-end:d2d-direct:1",
                "verify:1",
            ]
        );
        assert_eq!(
            contexts.into_inner(),
            [
                sample_context(TransferClass::D2DDirect, 4096, 0, BenchMode::Single),
                sample_context(TransferClass::D2DDirect, 4096, 1, BenchMode::Single),
            ]
        );
    }

    #[test]
    fn synthetic_staged_lifecycle_uses_one_observed_window_for_both_legs() {
        let log = RefCell::new(Vec::new());
        let contexts = RefCell::new(Vec::new());
        let mut observer = recording_observer(&log, &contexts);
        let context = sample_context(TransferClass::D2DStaged, 8192, 0, BenchMode::Saturation);

        log.borrow_mut().push("clear".to_owned());
        log.borrow_mut().push("prepare-d2h".to_owned());
        log.borrow_mut().push("prepare-h2d".to_owned());
        observe_sample(&mut observer, &context, || {
            log.borrow_mut().push("d2h-submit-sync".to_owned());
            log.borrow_mut().push("h2d-submit-sync".to_owned());
            Ok(Duration::from_nanos(11))
        })
        .expect("sample succeeds");
        log.borrow_mut().push("verify".to_owned());

        assert_eq!(
            log.into_inner(),
            [
                "clear",
                "prepare-d2h",
                "prepare-h2d",
                "observe-start:d2d-staged:0",
                "d2h-submit-sync",
                "h2d-submit-sync",
                "observe-end:d2d-staged:0",
                "verify",
            ]
        );
        assert_eq!(
            contexts.into_inner(),
            [sample_context(
                TransferClass::D2DStaged,
                8192,
                0,
                BenchMode::Saturation
            )]
        );
    }
}
