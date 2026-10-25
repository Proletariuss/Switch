//! The production three-listener accept/reap scheduler.
//!
//! A completed connection only releases its own observer slot. Business and
//! cleanup owners retain their independent tasks and receipts outside this
//! driver. Keeping the scheduler in one callable unit lets same-module tests
//! exercise the exact capacity and listener-selection behavior used by main.
use std::future::Future;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::{JoinError, JoinSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pool {
    Public,
    Callback,
    Internal,
}

impl Pool {
    pub(crate) fn accept_owner(self) -> &'static str {
        match self {
            Self::Public => "public accept",
            Self::Callback => "callback accept",
            Self::Internal => "internal accept",
        }
    }

    pub(crate) fn task_owner(self) -> &'static str {
        match self {
            Self::Public => "public task",
            Self::Callback => "callback task",
            Self::Internal => "internal task",
        }
    }
}

pub(crate) enum Event {
    Accepted(Pool, std::io::Result<UnixStream>),
    Completed(Pool, Result<(), JoinError>),
}

pub(crate) struct Driver {
    public: JoinSet<()>,
    callback: JoinSet<()>,
    internal: JoinSet<()>,
}

impl Default for Driver {
    fn default() -> Self {
        Self {
            public: JoinSet::new(),
            callback: JoinSet::new(),
            internal: JoinSet::new(),
        }
    }
}

impl Driver {
    /// Select one production scheduling event. UnixListener::accept and
    /// JoinSet::join_next are cancellation safe, so the daemon's outer select
    /// may continue to prioritize signals, parent loss and QMI completion.
    pub(crate) async fn next(
        &mut self,
        public: &UnixListener,
        callback: &UnixListener,
        internal: &UnixListener,
    ) -> Event {
        tokio::select! {
            accepted = public.accept(), if self.public.len() < crate::PUBLIC_CONNECTION_LIMIT => {
                Event::Accepted(Pool::Public, accepted.map(|(stream, _)| stream))
            }
            accepted = callback.accept(), if self.callback.len() < crate::NATIVE_CALLBACK_CONNECTION_LIMIT => {
                Event::Accepted(Pool::Callback, accepted.map(|(stream, _)| stream))
            }
            accepted = internal.accept(), if self.internal.len() < crate::INTERNAL_STORE_CONNECTION_LIMIT => {
                Event::Accepted(Pool::Internal, accepted.map(|(stream, _)| stream))
            }
            completed = self.public.join_next(), if !self.public.is_empty() => {
                Event::Completed(Pool::Public, completed.expect("enabled public join set"))
            }
            completed = self.callback.join_next(), if !self.callback.is_empty() => {
                Event::Completed(Pool::Callback, completed.expect("enabled callback join set"))
            }
            completed = self.internal.join_next(), if !self.internal.is_empty() => {
                Event::Completed(Pool::Internal, completed.expect("enabled internal join set"))
            }
        }
    }

    pub(crate) fn spawn<F>(&mut self, pool: Pool, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let tasks = match pool {
            Pool::Public => &mut self.public,
            Pool::Callback => &mut self.callback,
            Pool::Internal => &mut self.internal,
        };
        debug_assert!(tasks.len() < Self::limit(pool));
        tasks.spawn(future);
    }

    pub(crate) fn len(&self, pool: Pool) -> usize {
        match pool {
            Pool::Public => self.public.len(),
            Pool::Callback => self.callback.len(),
            Pool::Internal => self.internal.len(),
        }
    }

    pub(crate) fn limit(pool: Pool) -> usize {
        match pool {
            Pool::Public => crate::PUBLIC_CONNECTION_LIMIT,
            Pool::Callback => crate::NATIVE_CALLBACK_CONNECTION_LIMIT,
            Pool::Internal => crate::INTERNAL_STORE_CONNECTION_LIMIT,
        }
    }

    pub(crate) fn abort_all(&mut self) {
        self.public.abort_all();
        self.callback.abort_all();
        self.internal.abort_all();
    }

    pub(crate) async fn drain(&mut self, report: &mut crate::shutdown::Report) {
        crate::shutdown_connection_joins("public requests", &mut self.public, report).await;
        crate::shutdown_connection_joins("callback requests", &mut self.callback, report).await;
        crate::shutdown_connection_joins("internal requests", &mut self.internal, report).await;
    }
}

#[cfg(test)]
#[path = "connection_pools/tests.rs"]
mod tests;
