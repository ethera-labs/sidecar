//! Publisher period and `StartInstance` admission state.

use ethera_spec::{PeriodId, SequenceNumber};
use sidecar_primitives_traits::CoordinatorError;

/// Period-scoped state used to admit publisher-assigned instances.
///
/// The publisher's sequence number is global within a period. A sidecar may see
/// gaps for instances that do not include its chain, so admission only requires
/// strict advancement over the last sequence this sidecar accepted.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PublisherPeriod {
    current: Option<PeriodId>,
    last_sequence: Option<SequenceNumber>,
}

impl PublisherPeriod {
    pub(crate) fn start(&mut self, period_id: PeriodId) {
        self.current = Some(period_id);
        self.last_sequence = None;
    }

    pub(crate) fn close(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub(crate) fn current(&self) -> Option<PeriodId> {
        self.current
    }

    pub(crate) fn accept_start_instance(
        &mut self,
        period_id: PeriodId,
        sequence_number: SequenceNumber,
    ) -> Result<(), CoordinatorError> {
        let current = self.current.ok_or(CoordinatorError::PeriodNotInitialized)?;

        if period_id < current {
            return Err(CoordinatorError::StalePeriod {
                current,
                received: period_id,
            });
        }

        if period_id > current {
            return Err(CoordinatorError::FuturePeriod {
                current,
                received: period_id,
            });
        }

        if let Some(last) = self.last_sequence {
            if sequence_number <= last {
                return Err(CoordinatorError::StaleSequence {
                    last,
                    received: sequence_number,
                });
            }
        }

        self.last_sequence = Some(sequence_number);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_until_period_starts() {
        let mut period = PublisherPeriod::default();

        assert!(matches!(
            period.accept_start_instance(PeriodId(1), SequenceNumber(1)),
            Err(CoordinatorError::PeriodNotInitialized)
        ));
    }

    #[test]
    fn accepts_gapped_strictly_increasing_sequences() {
        let mut period = PublisherPeriod::default();
        period.start(PeriodId(7));

        period
            .accept_start_instance(PeriodId(7), SequenceNumber(11))
            .unwrap();
        period
            .accept_start_instance(PeriodId(7), SequenceNumber(19))
            .unwrap();
    }

    #[test]
    fn rejects_period_mismatch() {
        let mut period = PublisherPeriod::default();
        period.start(PeriodId(7));

        assert!(matches!(
            period.accept_start_instance(PeriodId(6), SequenceNumber(1)),
            Err(CoordinatorError::StalePeriod {
                current: PeriodId(7),
                received: PeriodId(6),
            })
        ));

        assert!(matches!(
            period.accept_start_instance(PeriodId(8), SequenceNumber(1)),
            Err(CoordinatorError::FuturePeriod {
                current: PeriodId(7),
                received: PeriodId(8),
            })
        ));
    }

    #[test]
    fn rejects_replayed_sequences() {
        let mut period = PublisherPeriod::default();
        period.start(PeriodId(7));
        period
            .accept_start_instance(PeriodId(7), SequenceNumber(3))
            .unwrap();

        assert!(matches!(
            period.accept_start_instance(PeriodId(7), SequenceNumber(3)),
            Err(CoordinatorError::StaleSequence {
                last: SequenceNumber(3),
                received: SequenceNumber(3),
            })
        ));
    }

    #[test]
    fn start_resets_sequence() {
        let mut period = PublisherPeriod::default();
        period.start(PeriodId(7));
        period
            .accept_start_instance(PeriodId(7), SequenceNumber(3))
            .unwrap();

        period.start(PeriodId(8));

        period
            .accept_start_instance(PeriodId(8), SequenceNumber(1))
            .unwrap();
    }
}
