//! Independent carrier-side CHILD key schedule; no daemon state injection.
use super::*;
use tokio::sync::oneshot;
pub(super) enum Command {
    Child {
        crossed: bool,
        done: oneshot::Sender<()>,
    },
    Lifetime {
        seconds: u32,
        done: oneshot::Sender<()>,
    },
}
pub(in super::super) struct Client(mpsc::Sender<Command>);
impl Client {
    pub(in super::super) async fn child(&self, crossed: bool) {
        let (done, result) = oneshot::channel();
        self.0.send(Command::Child { crossed, done }).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(8), result)
            .await
            .unwrap()
            .unwrap();
    }
    pub(in super::super) async fn lifetime(&self, seconds: u32) {
        let (done, result) = oneshot::channel();
        self.0
            .send(Command::Lifetime { seconds, done })
            .await
            .unwrap();
        result.await.unwrap();
    }
}
pub(super) fn new() -> (mpsc::Receiver<Command>, Client) {
    let (send, recv) = mpsc::channel(4);
    (recv, Client(send))
}
struct Pending {
    nonce: Vec<u8>,
    old_client: u32,
    old_server: u32,
    new_server: u32,
    crossed: bool,
    installed: Option<tokio::time::Instant>,
    first_delete: Option<Vec<u8>>,
    done: oneshot::Sender<()>,
}
pub(super) struct State {
    next_id: u32,
    client_spi: u32,
    server_spi: u32,
    pending: Option<Pending>,
    lifetime: Option<oneshot::Sender<()>>,
}
impl State {
    pub(super) fn new(client_spi: u32) -> Self {
        Self {
            next_id: 0,
            client_spi,
            server_spi: 2,
            pending: None,
            lifetime: None,
        }
    }
    fn header(&mut self, initiator_spi: u64, exchange: ExchangeType) -> IkeHeader {
        let id = self.next_id;
        self.next_id += 1;
        IkeHeader {
            initiator_spi,
            responder_spi: 22,
            next_payload: 46,
            version: 0x20,
            exchange,
            flags: 0,
            message_id: id,
        }
    }
    pub(super) fn command(
        &mut self,
        command: Command,
        initiator_spi: u64,
        keys: &proletarius_ike::IkeSaKeyMaterial,
    ) -> Vec<u8> {
        assert!(self.pending.is_none());
        let (exchange, frames) = match command {
            Command::Child { crossed, done } => {
                let new_server = self.server_spi + 101;
                let nonce = vec![new_server as u8; 32];
                let mut notice = vec![3, 4, 0x40, 0x09];
                notice.extend(self.server_spi.to_be_bytes());
                self.pending = Some(Pending {
                    nonce: nonce.clone(),
                    old_client: self.client_spi,
                    old_server: self.server_spi,
                    new_server,
                    crossed,
                    installed: None,
                    first_delete: None,
                    done,
                });
                (
                    ExchangeType::CreateChildSa,
                    vec![
                        esp_offer(NativeSuite::CtExcel, new_server),
                        frame(40, nonce),
                        frame(41, notice),
                        ts(44),
                        ts(45),
                    ],
                )
            }
            Command::Lifetime { seconds, done } => {
                self.lifetime = Some(done);
                (
                    ExchangeType::Informational,
                    vec![frame(
                        41,
                        [vec![0, 0, 0x40, 0x13], seconds.to_be_bytes().to_vec()].concat(),
                    )],
                )
            }
        };
        let header = self.header(initiator_spi, exchange);
        seal_ike_sk(
            NativeSuite::CtExcel,
            header,
            &keys.sk_er,
            &keys.sk_ar,
            &frames,
        )
        .unwrap()
    }
    /// Consumes only a matched protected exchange. Current IKE DELETE and DPD
    /// remain with the ordinary fixture, and every other unexpected packet fails.
    pub(super) async fn packet(
        &mut self,
        raw: &[u8],
        h: &IkeHeader,
        f: &[PayloadFrame],
        keys: &proletarius_ike::IkeSaKeyMaterial,
        udp: &UdpSocket,
        source: SocketAddr,
        rx: &mut EspReceiver,
        tx: &mut EspSender,
    ) -> bool {
        let suite = NativeSuite::CtExcel;
        if h.is_response() {
            assert_eq!(h.message_id, self.next_id - 1);
            if let Some(done) = self.lifetime.take() {
                assert!(f.is_empty());
                done.send(()).unwrap();
                return true;
            }
            if h.exchange == ExchangeType::CreateChildSa {
                let pending = self.pending.as_mut().unwrap();
                assert!(pending.installed.is_none());
                let proposal = proletarius_ike::decode_sa_proposals(
                    &f.iter().find(|v| v.kind == 33).unwrap().body,
                )
                .unwrap();
                assert_eq!(proposal.len(), 1);
                self.client_spi =
                    u32::from_be_bytes(proposal[0].spi.as_slice().try_into().unwrap());
                assert_ne!(self.client_spi, pending.old_client);
                let nonce = &f.iter().find(|v| v.kind == 40).unwrap().body;
                let mut child = proletarius_ike::derive_rekey_child_sa_keys(
                    5,
                    &keys.sk_d,
                    &[],
                    &pending.nonce,
                    nonce,
                    16,
                    32,
                )
                .unwrap();
                *rx = esp_receiver(
                    suite,
                    pending.new_server,
                    std::mem::take(&mut child.responder_encryption),
                    std::mem::take(&mut child.responder_integrity),
                );
                *tx = esp_sender(
                    suite,
                    self.client_spi,
                    std::mem::take(&mut child.initiator_encryption),
                    std::mem::take(&mut child.initiator_integrity),
                );
                self.server_spi = pending.new_server;
                pending.installed = Some(tokio::time::Instant::now());
                return true;
            }
            let pending = self.pending.take().unwrap();
            assert!(pending.crossed);
            assert_eq!(f.len(), 1);
            assert_eq!(f[0].kind, 42);
            assert_eq!(
                f[0].body,
                [vec![3, 4, 0, 1], pending.old_client.to_be_bytes().to_vec()].concat()
            );
            pending.done.send(()).unwrap();
            return true;
        }
        if let Some(delete) = f
            .iter()
            .find(|v| v.kind == 42 && v.body.first() == Some(&3))
        {
            let pending = self.pending.as_mut().unwrap();
            if !pending.crossed {
                if let Some(first) = &pending.first_delete {
                    assert_eq!(raw, first, "retirement retry changed encrypted request");
                } else {
                    pending.first_delete = Some(raw.to_vec());
                    return true;
                }
            }
            assert!(
                pending.installed.unwrap().elapsed() >= std::time::Duration::from_millis(2900),
                "old inbound grace shortened"
            );
            assert_eq!(
                delete.body,
                [vec![3, 4, 0, 1], pending.old_client.to_be_bytes().to_vec()].concat()
            );
            if pending.crossed {
                let body = [vec![3, 4, 0, 1], pending.old_server.to_be_bytes().to_vec()].concat();
                let header = self.header(h.initiator_spi, ExchangeType::Informational);
                let packet =
                    seal_ike_sk(suite, header, &keys.sk_er, &keys.sk_ar, &[frame(42, body)])
                        .unwrap();
                send(udp, source, 4500, &proletarius_ike::encode_natt(&packet)).await;
            }
            let mut response = h.clone();
            response.flags = IkeHeader::RESPONSE_FLAG;
            let packet = seal_ike_sk(suite, response, &keys.sk_er, &keys.sk_ar, &[]).unwrap();
            send(udp, source, 4500, &proletarius_ike::encode_natt(&packet)).await;
            if !self.pending.as_ref().unwrap().crossed {
                self.pending.take().unwrap().done.send(()).unwrap();
            }
            return true;
        }
        false
    }
}
