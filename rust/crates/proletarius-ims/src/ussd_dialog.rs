//! Pure USSD transaction decisions. SIP I/O and dialog side effects remain in
//! the transport owner; this module decides how an INVITE response advances.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InviteResponseAction {
    IgnoreProvisional,
    Reject,
    Establish,
    AcknowledgeEstablished,
    MissingRemoteTag,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboundRequestAction {
    QueuePending,
    RejectServiceUnavailable,
    RejectDialogMissing,
    IgnoreAck,
    RejectMethod,
    RejectBadRequest,
    AcknowledgeDuplicate,
    RejectInfoPackage,
    Accept,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandAction {
    RejectNotEstablished,
    RejectWaitingForNetwork,
    SendInput,
    Cancel,
}

pub fn command_action(established: bool, waiting_input: bool, cancel: bool) -> CommandAction {
    if !established {
        return CommandAction::RejectNotEstablished;
    }
    if cancel {
        return CommandAction::Cancel;
    }
    if !waiting_input {
        return CommandAction::RejectWaitingForNetwork;
    }
    CommandAction::SendInput
}

#[allow(clippy::too_many_arguments)]
pub fn inbound_request_action(
    established: bool,
    pending_count: usize,
    remote_tag_matches: bool,
    local_tag_matches: bool,
    method: &str,
    cseq_present: bool,
    cseq: u32,
    remote_seq: u32,
    info_package_valid: bool,
) -> InboundRequestAction {
    if !established {
        return if pending_count < 8 {
            InboundRequestAction::QueuePending
        } else {
            InboundRequestAction::RejectServiceUnavailable
        };
    }
    if !remote_tag_matches || !local_tag_matches {
        return InboundRequestAction::RejectDialogMissing;
    }
    if method == "ACK" {
        return InboundRequestAction::IgnoreAck;
    }
    if method != "INFO" && method != "BYE" {
        return InboundRequestAction::RejectMethod;
    }
    if !cseq_present {
        return InboundRequestAction::RejectBadRequest;
    }
    if cseq <= remote_seq {
        return InboundRequestAction::AcknowledgeDuplicate;
    }
    if method == "INFO" && !info_package_valid {
        return InboundRequestAction::RejectInfoPackage;
    }
    InboundRequestAction::Accept
}

pub fn invite_response_action(
    status: u16,
    established: bool,
    remote_tag_present: bool,
) -> InviteResponseAction {
    if status < 200 {
        return InviteResponseAction::IgnoreProvisional;
    }
    if status >= 300 {
        return InviteResponseAction::Reject;
    }
    if established {
        return InviteResponseAction::AcknowledgeEstablished;
    }
    if !remote_tag_present {
        return InviteResponseAction::MissingRemoteTag;
    }
    InviteResponseAction::Establish
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_response_policy_preserves_provisional_rejection_and_dialog_rules() {
        for status in [100, 180, 199] {
            assert_eq!(
                invite_response_action(status, false, false),
                InviteResponseAction::IgnoreProvisional
            );
        }
        for status in [300, 403, 486, 500, 699] {
            assert_eq!(
                invite_response_action(status, false, true),
                InviteResponseAction::Reject
            );
        }
        assert_eq!(
            invite_response_action(200, false, false),
            InviteResponseAction::MissingRemoteTag
        );
        assert_eq!(
            invite_response_action(200, false, true),
            InviteResponseAction::Establish
        );
        assert_eq!(
            invite_response_action(200, true, true),
            InviteResponseAction::AcknowledgeEstablished
        );
    }

    #[test]
    fn inbound_request_policy_preserves_queue_tag_method_cseq_and_package_order() {
        assert_eq!(
            inbound_request_action(false, 7, false, false, "BYE", false, 0, 0, false),
            InboundRequestAction::QueuePending
        );
        assert_eq!(
            inbound_request_action(false, 8, false, false, "INFO", false, 0, 0, false),
            InboundRequestAction::RejectServiceUnavailable
        );
        assert_eq!(
            inbound_request_action(true, 0, false, true, "INFO", false, 0, 0, false),
            InboundRequestAction::RejectDialogMissing
        );
        assert_eq!(
            inbound_request_action(true, 0, true, true, "ACK", false, 0, 0, false),
            InboundRequestAction::IgnoreAck
        );
        assert_eq!(
            inbound_request_action(true, 0, true, true, "OPTIONS", false, 0, 0, false),
            InboundRequestAction::RejectMethod
        );
        assert_eq!(
            inbound_request_action(true, 0, true, true, "BYE", false, 0, 0, false),
            InboundRequestAction::RejectBadRequest
        );
        assert_eq!(
            inbound_request_action(true, 0, true, true, "INFO", true, 4, 4, false),
            InboundRequestAction::AcknowledgeDuplicate
        );
        assert_eq!(
            inbound_request_action(true, 0, true, true, "INFO", true, 5, 4, false),
            InboundRequestAction::RejectInfoPackage
        );
        assert_eq!(
            inbound_request_action(true, 0, true, true, "BYE", true, 5, 4, false),
            InboundRequestAction::Accept
        );
    }

    #[test]
    fn command_policy_preserves_establishment_waiting_and_cancel_rules() {
        assert_eq!(
            command_action(false, true, true),
            CommandAction::RejectNotEstablished
        );
        assert_eq!(
            command_action(true, false, false),
            CommandAction::RejectWaitingForNetwork
        );
        assert_eq!(command_action(true, true, false), CommandAction::SendInput);
        assert_eq!(command_action(true, false, true), CommandAction::Cancel);
    }
}
