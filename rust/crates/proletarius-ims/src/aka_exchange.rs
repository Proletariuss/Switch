//! Cancellable SIM authentication without owning or selecting a physical reader.
use crate::{
    SipResponse,
    aka_authorization::{AuthorizationHeader, AuthorizationRequest, authorize, select_challenge},
    aka_digest::{AkaPassword, DigestError},
};
use std::future::Future;
use tokio::{sync::watch, time::Instant};
use zeroize::Zeroizing;

pub enum SimAnswer {
    Success {
        res: Zeroizing<Vec<u8>>,
        ck: Zeroizing<[u8; 16]>,
        ik: Zeroizing<[u8; 16]>,
    },
    SynchronizationFailure(Zeroizing<[u8; 14]>),
    Rejected,
}
pub struct SecurityKeys {
    pub ck: Zeroizing<[u8; 16]>,
    pub ik: Zeroizing<[u8; 16]>,
}
pub struct AuthenticatedChallenge {
    header: AuthorizationHeader,
    keys: Option<SecurityKeys>,
}

impl AuthenticatedChallenge {
    pub fn header(&self) -> &AuthorizationHeader {
        &self.header
    }

    pub fn security_keys(&self) -> Option<&SecurityKeys> {
        self.keys.as_ref()
    }

    pub(crate) fn into_parts(self) -> (AuthorizationHeader, Option<SecurityKeys>) {
        (self.header, self.keys)
    }

    #[cfg(test)]
    pub(crate) fn from_parts(header: AuthorizationHeader, keys: Option<SecurityKeys>) -> Self {
        Self { header, keys }
    }
}
#[derive(Debug)]
pub enum ExchangeError<E> {
    Digest(DigestError),
    Sim(E),
    Rejected,
    Cancelled,
    Deadline,
}

/// The callback must be bound to the session's immutable reader/SIM identity.
/// No detached task is spawned: dropping this future drops the pending callback.
pub async fn answer_challenge<F, Fut, E>(
    response: &SipResponse,
    request: &AuthorizationRequest<'_>,
    deadline: Instant,
    mut stop: watch::Receiver<bool>,
    authenticate: F,
) -> Result<AuthenticatedChallenge, ExchangeError<E>>
where
    F: FnOnce(Zeroizing<[u8; 16]>, Zeroizing<[u8; 16]>) -> Fut,
    Fut: Future<Output = Result<SimAnswer, E>>,
{
    if *stop.borrow() || stop.has_changed().is_err() {
        return Err(ExchangeError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(ExchangeError::Deadline);
    }
    let (name, challenge) = select_challenge(response).map_err(ExchangeError::Digest)?;
    // Validate all credential inputs before touching hardware. This throwaway
    // computation uses no SIM secret and is never sent or logged.
    drop(authorize(challenge, request, AkaPassword::Res(&[0; 4])).map_err(ExchangeError::Digest)?);
    let parameters = crate::digest_parameters::parse(challenge)
        .ok_or(ExchangeError::Digest(DigestError::InvalidInput))?;
    let nonce = parameters
        .iter()
        .find(|(key, _)| key == "nonce")
        .ok_or(ExchangeError::Digest(DigestError::InvalidNonce))?;
    let nonce = crate::aka_nonce::decode_aka_nonce(&nonce.1)
        .map_err(|_| ExchangeError::Digest(DigestError::InvalidNonce))?;
    let future = authenticate(nonce.rand, nonce.autn);
    tokio::pin!(future);
    let answer = loop {
        if *stop.borrow() || stop.has_changed().is_err() {
            return Err(ExchangeError::Cancelled);
        }
        tokio::select! {
            biased;
            _=stop.changed()=>continue,
            _=tokio::time::sleep_until(deadline)=>return Err(ExchangeError::Deadline),
            result=&mut future=>break result.map_err(ExchangeError::Sim)?,
        }
    };
    let (value, keys) = match answer {
        SimAnswer::Success { res, ck, ik } => (
            authorize(challenge, request, AkaPassword::Res(&res)).map_err(ExchangeError::Digest)?,
            Some(SecurityKeys { ck, ik }),
        ),
        SimAnswer::SynchronizationFailure(auts) => (
            authorize(
                challenge,
                request,
                AkaPassword::SynchronizationFailure(&auts),
            )
            .map_err(ExchangeError::Digest)?,
            None,
        ),
        SimAnswer::Rejected => return Err(ExchangeError::Rejected),
    };
    Ok(AuthenticatedChallenge {
        header: AuthorizationHeader::from_value(name, value),
        keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[tokio::test]
    async fn sim_result_and_keys_survive_only_successful_exchange() {
        let response = SipResponse::parse(format!("SIP/2.0 401 Challenge\r\nWWW-Authenticate: Digest realm=\"ims.test\",nonce=\"{}\",algorithm=AKAv1-MD5\r\nContent-Length: 0\r\n\r\n", "01".repeat(32)).as_bytes()).unwrap();
        let request = AuthorizationRequest {
            username: "u",
            method: "REGISTER",
            uri: "sip:ims.test",
            body: b"",
            cnonce: "c",
            nonce_count: 1,
        };
        let (stop_tx, stop) = watch::channel(false);
        let deadline = Instant::now() + Duration::from_secs(1);
        let answer = answer_challenge(
            &response,
            &request,
            deadline,
            stop.clone(),
            |rand, autn| async move {
                assert_eq!(*rand, [1; 16]);
                assert_eq!(*autn, [1; 16]);
                Ok::<_, ()>(SimAnswer::Success {
                    res: Zeroizing::new(vec![1; 8]),
                    ck: Zeroizing::new([2; 16]),
                    ik: Zeroizing::new([3; 16]),
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(answer.header.name(), "Authorization");
        assert_eq!(*answer.keys.unwrap().ck, [2; 16]);
        let stalled = answer_challenge(
            &response,
            &request,
            Instant::now() + Duration::from_millis(5),
            stop.clone(),
            |_, _| std::future::pending::<Result<SimAnswer, ()>>(),
        )
        .await;
        assert!(matches!(stalled, Err(ExchangeError::Deadline)));
        stop_tx.send(true).unwrap();
        let cancelled = answer_challenge(&response, &request, deadline, stop, |_, _| async {
            panic!("cancelled operation touched SIM");
            #[allow(unreachable_code)]
            Ok::<_, ()>(SimAnswer::Rejected)
        })
        .await;
        assert!(matches!(cancelled, Err(ExchangeError::Cancelled)));
    }
}
