use super::*;
use crate::{auxiliary::*, tokens::TokenCount};
pub(crate) struct AuxiliaryLease<'a> {
    limiter: &'a RunLimiter,
    index: usize,
    reserved: u64,
    started: bool,
    settled: bool,
    pub reporter: AuxiliaryUsageReporter,
}
impl RunLimiter {
    pub(crate) async fn auxiliary_slot(&self) -> anyhow::Result<tokio::sync::SemaphorePermit<'_>> {
        tokio::select! {biased;cancelled=self.context.cancelled()=>Err(cancelled.into()),permit=self.auxiliary_gate.acquire()=>permit.map_err(anyhow::Error::from)}
    }
    pub(crate) fn reserve_auxiliary<'a>(
        &'a self,
        model: &ClassifierModel,
        call_id: &str,
        input_count: TokenCount,
    ) -> anyhow::Result<AuxiliaryLease<'a>> {
        self.check()?;
        let reserved = input_count
            .tokens
            .saturating_add(u64::from(model.output_reservation));
        anyhow::ensure!(
            reserved <= model.context_tokens,
            "auxiliary request exceeds known model window"
        );
        let mut state = self.state.lock().expect("run budget poisoned");
        let calls = state.auxiliary.len();
        let maximum = self
            .budget
            .max_auxiliary_calls
            .unwrap_or(MAX_AUXILIARY_CALLS)
            .min(MAX_AUXILIARY_CALLS);
        if calls >= maximum {
            let stop = BudgetExceeded {
                resource: BudgetResource::AuxiliaryCalls,
                limit: maximum as u64,
                used: calls as u64,
                requested: Some(1),
            };
            drop(state);
            self.stop(stop.clone());
            return Err(stop.into());
        }
        if let Some(limit) = self.budget.max_tokens
            && reserved
                > limit
                    .saturating_sub(state.tokens.used)
                    .saturating_sub(state.auxiliary_reserved)
        {
            let stop = BudgetExceeded {
                resource: BudgetResource::Tokens,
                limit,
                used: state.tokens.used,
                requested: Some(reserved),
            };
            drop(state);
            self.stop(stop.clone());
            return Err(stop.into());
        }
        state.auxiliary_reserved = state.auxiliary_reserved.saturating_add(reserved);
        state.auxiliary.push(AuxiliaryAttempt {
            id: uuid::Uuid::new_v4().to_string(),
            call_id: call_id.into(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            status: "prepared".into(),
            usage: ModelUsage::default(),
            reported_usage: false,
            tokens: TokenAccounting::default(),
            input_count,
            delivery_complete: false,
        });
        Ok(AuxiliaryLease {
            limiter: self,
            index: calls,
            reserved,
            started: false,
            settled: false,
            reporter: AuxiliaryUsageReporter::default(),
        })
    }
}
impl AuxiliaryLease<'_> {
    pub fn attempt(&self) -> AuxiliaryAttempt {
        self.limiter
            .state
            .lock()
            .expect("run budget poisoned")
            .auxiliary[self.index]
            .clone()
    }
    pub fn start(&mut self) {
        self.started = true;
        self.limiter
            .state
            .lock()
            .expect("run budget poisoned")
            .auxiliary[self.index]
            .status = "running".into();
    }
    pub fn finish(&mut self, status: &str) -> AuxiliaryAttempt {
        if !self.settled {
            let usage = self.reporter.usage();
            let accounting = if self.started {
                TokenAccounting::from_usage(&usage.clone().unwrap_or_default(), self.reserved)
            } else {
                TokenAccounting::default()
            };
            let mut state = self.limiter.state.lock().expect("run budget poisoned");
            state.auxiliary_reserved = state.auxiliary_reserved.saturating_sub(self.reserved);
            state.tokens.used = state.tokens.used.saturating_add(accounting.used);
            state.tokens.estimated |= accounting.estimated;
            let attempt = &mut state.auxiliary[self.index];
            attempt.status = if self.started { status } else { "not_started" }.into();
            attempt.reported_usage = usage.as_ref().is_some_and(usage_reported);
            attempt.usage = usage.unwrap_or_default();
            attempt.tokens = accounting;
            self.settled = true;
        }
        self.attempt()
    }
    pub fn delivered(&self) {
        self.limiter
            .state
            .lock()
            .expect("run budget poisoned")
            .auxiliary[self.index]
            .delivery_complete = true;
    }
}
impl Drop for AuxiliaryLease<'_> {
    fn drop(&mut self) {
        self.finish("interrupted");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::{TokenCount, TokenScope};
    fn model() -> ClassifierModel {
        ClassifierModel {
            provider: "fixture".into(),
            id: "native".into(),
            name: "native".into(),
            images: false,
            context_tokens: 1000,
            output_reservation: 0,
        }
    }
    fn count() -> TokenCount {
        TokenCount::estimated(100, TokenScope::Request, "fixture")
    }
    #[test]
    fn concurrent_reservations_cannot_spend_the_same_remaining_parent_tokens() {
        let limits = RunLimiter::new(
            RunBudget {
                max_tokens: Some(250),
                ..Default::default()
            },
            TurnExecutionContext::new(),
        );
        let mut first = limits.reserve_auxiliary(&model(), "one", count()).unwrap();
        first.start();
        let mut second = limits.reserve_auxiliary(&model(), "two", count()).unwrap();
        second.start();
        assert_eq!(limits.remaining_tokens(), Some(50));
        assert!(
            limits
                .reserve_auxiliary(&model(), "three", count())
                .is_err()
        );
        first.reporter.record(ModelUsage {
            input_tokens: Some(60),
            output_tokens: Some(0),
            total_tokens: Some(60),
            cached_input_tokens: Some(0),
            cache_write_tokens: Some(0),
        });
        first.finish("completed");
        drop(second);
        let mut progress = RunProgress::new(vec![]);
        progress.usage = ModelUsage::zero();
        limits.snapshot(&mut progress);
        assert_eq!(progress.tokens.used, 160);
        assert!(progress.tokens.estimated);
        assert_eq!(progress.auxiliary.len(), 2);
        assert_eq!(progress.auxiliary[1].status, "interrupted");
        assert!(progress.usage.total_tokens.is_none());
        limits.snapshot(&mut progress);
        assert_eq!(progress.tokens.used, 160);
        assert_eq!(progress.auxiliary.len(), 2);
    }
    #[test]
    fn unknown_task_usage_stays_estimated_and_known_windows_precede_provider_invocation() {
        let limits = RunLimiter::new(RunBudget::default(), TurnExecutionContext::new());
        let mut small = model();
        small.context_tokens = 99;
        assert!(limits.reserve_auxiliary(&small, "small", count()).is_err());
        let mut lease = limits.reserve_auxiliary(&model(), "task", count()).unwrap();
        lease.start();
        drop(lease);
        let mut progress = RunProgress::new(vec![]);
        limits.snapshot(&mut progress);
        assert_eq!(progress.tokens.used, 100);
        assert!(progress.tokens.estimated);
        assert!(!progress.auxiliary[0].reported_usage);
        assert_eq!(
            progress.primary_usage.as_ref().unwrap().total_tokens,
            Some(0)
        );
    }
}
