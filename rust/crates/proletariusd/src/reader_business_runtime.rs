//! Retirement evidence for capabilities issued before native ownership.
//! No Go status or RPC acknowledgement may release an unknown reader owner.
use crate::message_archive::InboxState;

#[derive(Clone)]
pub(crate) struct Bridge {
    inbox: InboxState,
}
impl Bridge {
    pub(crate) fn new(inbox: InboxState) -> Self {
        Self { inbox }
    }
    pub(crate) fn producer_retired(&self, captured: &str) -> bool {
        // Replacement is published only after the child has actually exited.
        // An absent producer or unavailable Go socket is not completion proof.
        self.inbox
            .current_go_producer_id()
            .is_some_and(|current| current != captured)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_confirmed_producer_replacement_retires_captured_owner() {
        let inbox = InboxState::for_go_producer("original".into());
        let observer = Bridge::new(inbox.clone());
        assert!(!observer.producer_retired("original"));
        inbox
            .replace_go_producer("confirmed-child-exit".into())
            .unwrap();
        assert!(observer.producer_retired("original"));
        assert!(!observer.producer_retired("confirmed-child-exit"));
    }
}
