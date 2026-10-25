//! Session-bound card capability. Ordinary sessions retain their existing lease;
//! N-wait sessions retain identity and authority, borrowing physical work only
//! for the duration of the shared native card workflow.
use crate::reader_session_lease::{
    LeaseIdentity, NativeReaderSessionHeartbeat, NativeReaderSessionLease, ReaderAkaApplication,
};
use proletarius_domain::BackendFailure;
use proletarius_vowifi::SimAkaResult;
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::watch;

pub(crate) type CardFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, BackendFailure>> + Send + 'static>>;
/// Source is captured by the physical capability, never by a public caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionKind {
    Reader,
    Module,
}
/// Each implementation binds one immutable profile/session generation. It must
/// submit AKA through the retained 493 physical workflow, and release must join
/// this session's submitted card work without releasing the shared pool owner.
pub(crate) trait OnDemandCard: Send + Sync {
    fn kind(&self) -> SessionKind {
        SessionKind::Reader
    }
    fn identity_current(&self) -> bool {
        false
    }
    fn reader(&self) -> &str;
    fn reader_generation(&self) -> u64;
    fn identity(&self) -> &LeaseIdentity;
    fn authenticate(
        self: Arc<Self>,
        application: ReaderAkaApplication,
        rand: [u8; 16],
        autn: [u8; 16],
    ) -> CardFuture<SimAkaResult>;
    fn lost_receiver(&self) -> watch::Receiver<bool>;
    fn release_with_confirmation(self: Arc<Self>) -> CardFuture<()>;
}
#[derive(Clone)]
pub(crate) enum SessionCard {
    Leased(Arc<NativeReaderSessionLease>),
    OnDemand(Arc<dyn OnDemandCard>),
}
impl SessionCard {
    pub(crate) fn identity_current(&self) -> bool {
        match self {
            Self::Leased(_) => false,
            Self::OnDemand(card) => card.identity_current(),
        }
    }
    pub(crate) fn kind(&self) -> SessionKind {
        match self {
            Self::Leased(_) => SessionKind::Reader,
            Self::OnDemand(card) => card.kind(),
        }
    }
    pub(crate) fn leased(lease: Arc<NativeReaderSessionLease>) -> Self {
        Self::Leased(lease)
    }
    pub(crate) fn on_demand(card: Arc<dyn OnDemandCard>) -> Self {
        Self::OnDemand(card)
    }
    pub(crate) fn reader(&self) -> &str {
        match self {
            Self::Leased(v) => &v.reader,
            Self::OnDemand(v) => v.reader(),
        }
    }
    pub(crate) fn reader_generation(&self) -> u64 {
        match self {
            Self::Leased(v) => v.reader_generation,
            Self::OnDemand(v) => v.reader_generation(),
        }
    }
    pub(crate) fn identity(&self) -> &LeaseIdentity {
        match self {
            Self::Leased(v) => &v.identity,
            Self::OnDemand(v) => v.identity(),
        }
    }
    pub(crate) fn ordinary_lease(&self) -> Option<&Arc<NativeReaderSessionLease>> {
        match self {
            Self::Leased(v) => Some(v),
            Self::OnDemand(_) => None,
        }
    }
    pub(crate) async fn authenticate(
        &self,
        application: ReaderAkaApplication,
        rand: &[u8; 16],
        autn: &[u8; 16],
    ) -> Result<SimAkaResult, BackendFailure> {
        match self {
            Self::Leased(v) => v.authenticate(application, rand, autn).await,
            Self::OnDemand(v) => v.clone().authenticate(application, *rand, *autn).await,
        }
    }
    pub(crate) async fn authenticate_ike(
        &self,
        rand: &[u8; 16],
        autn: &[u8; 16],
    ) -> Result<SimAkaResult, BackendFailure> {
        self.authenticate(ReaderAkaApplication::Usim, rand, autn)
            .await
    }
    pub(crate) async fn authenticate_ims(
        &self,
        rand: &[u8; 16],
        autn: &[u8; 16],
    ) -> Result<SimAkaResult, BackendFailure> {
        self.authenticate(ReaderAkaApplication::Isim, rand, autn)
            .await
    }
    pub(crate) async fn release_with_confirmation(&self) -> Result<(), BackendFailure> {
        match self {
            Self::Leased(v) => v.release_with_confirmation().await,
            Self::OnDemand(v) => v.clone().release_with_confirmation().await,
        }
    }
    pub(crate) async fn release(&self) -> Result<(), BackendFailure> {
        self.release_with_confirmation().await
    }
    pub(crate) fn heartbeat(&self) -> Heartbeat {
        match self {
            Self::Leased(v) => Heartbeat::Leased(NativeReaderSessionHeartbeat::start(v.clone())),
            Self::OnDemand(v) => Heartbeat::OnDemand {
                lost: v.lost_receiver(),
                _card: v.clone(),
            },
        }
    }
}
pub(crate) enum Heartbeat {
    Leased(NativeReaderSessionHeartbeat),
    OnDemand {
        lost: watch::Receiver<bool>,
        _card: Arc<dyn OnDemandCard>,
    },
}
impl Heartbeat {
    pub(crate) fn lost_receiver(&self) -> watch::Receiver<bool> {
        match self {
            Self::Leased(v) => v.lost_receiver(),
            Self::OnDemand { lost, .. } => lost.clone(),
        }
    }
    pub(crate) async fn wait_lost(&mut self) {
        match self {
            Self::Leased(v) => v.wait_lost().await,
            Self::OnDemand { lost, .. } => loop {
                if *lost.borrow() || lost.changed().await.is_err() {
                    return;
                }
            },
        }
    }
    pub(crate) async fn stop(self) {
        if let Self::Leased(v) = self {
            v.stop().await;
        }
    }
}

impl From<Arc<NativeReaderSessionLease>> for SessionCard {
    fn from(value: Arc<NativeReaderSessionLease>) -> Self {
        Self::leased(value)
    }
}
