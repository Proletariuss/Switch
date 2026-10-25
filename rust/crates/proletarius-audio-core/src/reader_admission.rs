//! Bounded reader VoWiFi host-audio admission. This is a decision only:
//! the Swift host still owns call identity, hangup fences and audio devices.

const SNAPSHOT_LEN: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Intent {
    Closed = 0,
    ReceiveOnly = 1,
    ReceiveWithLocalRingback = 2,
    IncomingReceiveOnly = 3,
    Duplex = 4,
}

impl Intent {
    // Route recovery intentionally excludes ordinary early media and ringback.
    // A fifth wire value distinguishes incoming receive-only from early RX
    // without changing the four audio modes exposed to the host.
    #[cfg(test)]
    const fn allows_route_recovery(self) -> bool {
        matches!(self, Self::IncomingReceiveOnly | Self::Duplex)
    }
}

fn decide(bytes: &[u8]) -> Intent {
    let Ok(
        [
            display,
            raw,
            direction,
            signaling,
            media,
            early_media,
            endpoint,
        ],
    ) = <&[u8; SNAPSHOT_LEN]>::try_from(bytes)
    else {
        return Intent::Closed;
    };
    if !matches!(*display, 0..=3)
        || !matches!(*raw, 0..=3)
        || !matches!(*direction, 0..=2)
        || [signaling, media, early_media, endpoint]
            .into_iter()
            .any(|flag| *flag > 1)
        || *signaling == 0
        || *endpoint == 0
    {
        return Intent::Closed;
    }

    match (*display, *raw, *direction) {
        // Displayed active alone is insufficient; wait for ready raw media.
        (3, 3, _) if *media == 1 => Intent::Duplex,
        // An incoming offerless INVITE can play audio before mediaReady.
        (1, 2, 1) => Intent::IncomingReceiveOnly,
        // Do not require outgoing direction or mediaReady for early media.
        (1 | 2, 1, _) if *early_media == 0 => Intent::ReceiveWithLocalRingback,
        (1 | 2, 1, _) => Intent::ReceiveOnly,
        _ => Intent::Closed,
    }
}

/// Seven one-byte fields: displayed state (0 other, 1 dialing, 2 alerting,
/// 3 active); raw state (0 other, 1 early, 2 connecting, 3 active); direction
/// (0 other, 1 incoming, 2 outgoing); then strict 0/1 signalingReady,
/// mediaReady, earlyMedia, valid audio endpoint. Invalid input fails closed.
/// Output: 0 closed, 1 receive-only, 2 receive with local ringback,
/// 3 incoming receive-only, 4 duplex. Value 3 is the receive-only subtype
/// eligible for route recovery; ordinary early media is not.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_reader_admission(snapshot: *const u8, length: usize) -> u8 {
    if snapshot.is_null() || length != SNAPSHOT_LEN {
        return Intent::Closed as u8;
    }
    let input = unsafe { std::slice::from_raw_parts(snapshot, length) };
    decide(input) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(display: u8, raw: u8, direction: u8, media: u8, early: u8) -> [u8; 7] {
        [display, raw, direction, 1, media, early, 1]
    }

    #[test]
    fn full_state_table_preserves_reader_host_audio_admission() {
        for display in 0..=3 {
            for raw in 0..=3 {
                for direction in 0..=2 {
                    for media in 0..=1 {
                        for early in 0..=1 {
                            let observed = decide(&snapshot(display, raw, direction, media, early));
                            let expected = if display == 3 && raw == 3 && media == 1 {
                                Intent::Duplex
                            } else if display == 1 && raw == 2 && direction == 1 {
                                Intent::IncomingReceiveOnly
                            } else if matches!(display, 1 | 2) && raw == 1 {
                                if early == 1 {
                                    Intent::ReceiveOnly
                                } else {
                                    Intent::ReceiveWithLocalRingback
                                }
                            } else {
                                Intent::Closed
                            };
                            assert_eq!(
                                observed, expected,
                                "{display}/{raw}/{direction}/{media}/{early}"
                            );
                            assert_eq!(
                                observed.allows_route_recovery(),
                                matches!(observed, Intent::Duplex | Intent::IncomingReceiveOnly)
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn early_media_and_incoming_connecting_keep_distinct_recovery() {
        let outgoing_early = snapshot(1, 1, 2, 0, 0);
        assert_eq!(decide(&outgoing_early), Intent::ReceiveWithLocalRingback);
        assert!(!decide(&outgoing_early).allows_route_recovery());
        assert_eq!(decide(&snapshot(2, 1, 0, 0, 1)), Intent::ReceiveOnly);
        assert_eq!(
            decide(&snapshot(1, 2, 1, 0, 0)),
            Intent::IncomingReceiveOnly
        );
        assert_eq!(decide(&snapshot(2, 2, 1, 0, 0)), Intent::Closed);
    }

    #[test]
    fn ffi_null_short_invalid_and_missing_readiness_fail_closed() {
        assert_eq!(
            unsafe { switch_audio_reader_admission(std::ptr::null(), 7) },
            0
        );
        let good = snapshot(3, 3, 0, 1, 0);
        assert_eq!(
            unsafe { switch_audio_reader_admission(good.as_ptr(), 0) },
            0
        );
        assert_eq!(
            unsafe { switch_audio_reader_admission(good.as_ptr(), 6) },
            0
        );
        assert_eq!(
            unsafe { switch_audio_reader_admission(good.as_ptr(), 7) },
            4
        );
        for index in 0..7 {
            let mut bad = good;
            bad[index] = 255;
            assert_eq!(unsafe { switch_audio_reader_admission(bad.as_ptr(), 7) }, 0);
        }
        for index in [3, 6] {
            let mut not_ready = good;
            not_ready[index] = 0;
            assert_eq!(
                unsafe { switch_audio_reader_admission(not_ready.as_ptr(), 7) },
                0
            );
        }
    }
}
