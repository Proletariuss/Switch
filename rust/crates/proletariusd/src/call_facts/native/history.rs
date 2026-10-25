//! A failed history owner cannot prevent the already registered IMS service
//! from communicating. An unavailable lane never claims another owner's facts.
use super::*;

const UNAVAILABLE: &str = "reader_ims: 读卡器通话历史记录不可用，历史观察任务未启动";

#[derive(Clone)]
pub(crate) enum NativeHistory {
    Active(NativeSink),
    Unavailable(Arc<str>),
}

impl NativeHistory {
    pub(crate) fn start_module_ims(facts: CallFacts, producer_id: String, capacity: usize) -> Self {
        let started = NativeSink::start_module_ims(facts.clone(), producer_id, capacity);
        match started {
            Ok(sink) => Self::Active(sink),
            Err(_) => Self::Unavailable(facts.register_native_startup_error_for(
                "module_ims: 模块通话历史记录不可用，历史观察任务未启动",
            )),
        }
    }
    pub(crate) fn start_reader_ims(facts: CallFacts, producer_id: String, capacity: usize) -> Self {
        let started = NativeSink::start_reader_ims(facts.clone(), producer_id, capacity);
        Self::from_start(&facts, started)
    }

    fn from_start(facts: &CallFacts, started: Result<NativeSink, String>) -> Self {
        match started {
            Ok(sink) => Self::Active(sink),
            Err(_) => {
                // Do not latch onto the occupied source: its producer, frozen
                // head and diagnostics belong to the earlier owner. Keep a
                // separate lifetime-bound diagnostic visible to history APIs.
                Self::Unavailable(facts.register_native_startup_error())
            }
        }
    }

    pub(crate) fn try_push(&self, fact: NativeFact) -> Result<(), String> {
        match self {
            Self::Active(sink) => sink.try_push(fact),
            Self::Unavailable(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn retire(&self, at: OffsetDateTime) -> Result<(), String> {
        match self {
            Self::Active(sink) => sink.retire(at),
            Self::Unavailable(error) => Err(error.to_string()),
        }
    }

    pub(crate) async fn drain(&self) -> Result<(), String> {
        match self {
            Self::Active(sink) => sink.drain().await,
            Self::Unavailable(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn worker_retired(&self) -> bool {
        match self {
            Self::Active(sink) => sink.worker_retired(),
            // This lane never acquired a source or created a worker. It owns
            // nothing to join; this is not evidence about the previous lane.
            // retire/drain still return Err, preserving archive failure.
            Self::Unavailable(_) => true,
        }
    }
}

impl CallFacts {
    fn register_native_startup_error(&self) -> Arc<str> {
        self.register_native_startup_error_for(UNAVAILABLE)
    }
    fn register_native_startup_error_for(&self, message: &'static str) -> Arc<str> {
        let error: Arc<str> = Arc::from(message);
        let mut errors = self
            .native_startup_errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        errors.retain(|entry| entry.strong_count() > 0);
        errors.push(Arc::downgrade(&error));
        error
    }

    pub(in crate::call_facts) fn native_startup_errors(&self) -> Vec<String> {
        let mut errors = self
            .native_startup_errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut messages = Vec::new();
        errors.retain(|entry| {
            let Some(error) = entry.upgrade() else {
                return false;
            };
            if !messages.iter().any(|message| message == error.as_ref()) {
                messages.push(error.to_string());
            }
            true
        });
        messages
    }
}

#[cfg(test)]
mod tests;
