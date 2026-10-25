use super::*;
use alloc::vec;
use core::convert::Infallible;

struct RecordingCard {
    operation: u8,
    result: u8,
    commands: Vec<Vec<u8>>,
}

impl CardTransport for RecordingCard {
    type Error = Infallible;

    fn transmit(
        &mut self,
        command: &[u8],
        response: &mut Vec<u8>,
    ) -> core::result::Result<(), Self::Error> {
        self.commands.push(command.to_vec());
        response.clear();
        response.extend_from_slice(&[0xbf, self.operation, 3, 0x80, 1, self.result, 0x90, 0]);
        Ok(())
    }
}

fn card(operation: u8, result: u8) -> Euicc<RecordingCard> {
    Euicc::new(
        RecordingCard {
            operation,
            result,
            commands: Vec::new(),
        },
        1,
        DEFAULT_MSS,
    )
    .unwrap()
}

#[test]
fn strict_enable_preserves_wrong_state_with_identical_refresh_false_wire() {
    let mut strict = card(0x31, 2);
    let mut compatibility = card(0x31, 2);
    let identifier = ProfileIdentifier::Iccid(&[0x98, 0x10]);
    assert!(matches!(
        strict.enable_profile_command_strict(identifier, false),
        Err(Error::ProfileOperation {
            operation: ProfileOperation::Enable,
            result: ProfileOperationResult::ProfileInWrongState,
        })
    ));
    compatibility
        .enable_profile_command(identifier, false)
        .unwrap();
    let expected = vec![vec![
        0x81, 0xe2, 0x91, 0, 12, 0xbf, 0x31, 9, 0xa0, 4, 0x5a, 2, 0x98, 0x10, 0x81, 1, 0,
    ]];
    assert_eq!(strict.into_inner().commands, expected);
    assert_eq!(compatibility.into_inner().commands, expected);
}

#[test]
fn strict_enable_accepts_zero_and_preserves_other_card_results() {
    for result in [0, 1, 3, 4, 5, 6, 7, 9, 10, 127, 42] {
        let mut strict = card(0x31, result);
        let outcome =
            strict.enable_profile_command_strict(ProfileIdentifier::Iccid(&[0x98]), false);
        if result == 0 {
            outcome.unwrap();
        } else {
            assert!(matches!(outcome, Err(Error::ProfileOperation {
                operation: ProfileOperation::Enable, result: actual,
            }) if actual.code() == result));
        }
        assert_eq!(strict.into_inner().commands.len(), 1);
    }
}

#[test]
fn compatibility_disable_still_accepts_wrong_state() {
    let mut compatibility = card(0x32, 2);
    compatibility
        .disable_profile_command(ProfileIdentifier::Iccid(&[0x98, 0x10]), false)
        .unwrap();
    assert_eq!(
        compatibility.into_inner().commands,
        vec![vec![
            0x81, 0xe2, 0x91, 0, 12, 0xbf, 0x32, 9, 0xa0, 4, 0x5a, 2, 0x98, 0x10, 0x81, 1, 0,
        ]]
    );
}
