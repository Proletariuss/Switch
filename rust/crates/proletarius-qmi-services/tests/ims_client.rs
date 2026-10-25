use proletarius_qmi_core::{Client, ClientOptions, Transport};
use proletarius_qmi_services::{
    IMS_GET_SERVICES, IMS_SET_SERVICES, IMSA_GET_REGISTRATION, ImsClient, ImsSettingsUpdate,
    NAS_GET_OPERATOR_NAME, NasClient, SERVICE_IMS, SERVICE_IMSA, SERVICE_NAS, SERVICE_UIM,
    UIM_GET_FILE_ATTRIBUTES, UIM_READ_RECORD, UIM_READ_TRANSPARENT, UimClient,
};
use proletarius_qmi_wire::{PacketMeta, Tlv, decode_packet, encode_packet};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

struct MemoryTransport {
    incoming: Mutex<Receiver<Vec<u8>>>,
    incoming_wake: Sender<Vec<u8>>,
    outgoing: SyncSender<Vec<u8>>,
    closed: Arc<AtomicBool>,
}

impl Transport for MemoryTransport {
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "closed"));
        }
        match self
            .incoming
            .lock()
            .expect("incoming lock")
            .recv_timeout(Duration::from_millis(20))
        {
            Ok(frame) => {
                if frame.len() > buffer.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "frame too large",
                    ));
                }
                buffer[..frame.len()].copy_from_slice(&frame);
                Ok(frame.len())
            }
            Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "no fixture frame",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "fixture closed",
            )),
        }
    }

    fn write_all(&self, data: &[u8]) -> io::Result<()> {
        self.outgoing
            .send(data.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "fixture closed"))
    }

    fn shutdown(&self) -> io::Result<()> {
        self.closed.store(true, Ordering::Release);
        let _ = self.incoming_wake.send(Vec::new());
        Ok(())
    }
}

#[test]
fn ims_client_allocates_uses_and_releases_two_service_ids() {
    let (client, requests, responses) = fake_client();
    let open_client = client.clone();
    let opening = thread::spawn(move || ImsClient::open(&open_client));

    let ims_allocate = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_allocate(&ims_allocate, SERVICE_IMS);
    responses
        .send(response(
            &ims_allocate,
            &[Tlv::new(0x02, &[0, 0, 0, 0]), Tlv::new(0x01, &[0x12, 0x07])],
        ))
        .unwrap();

    let imsa_allocate = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_allocate(&imsa_allocate, SERVICE_IMSA);
    responses
        .send(response(
            &imsa_allocate,
            &[Tlv::new(0x02, &[0, 0, 0, 0]), Tlv::new(0x01, &[0x21, 0x09])],
        ))
        .unwrap();
    let ims = opening.join().unwrap().unwrap();

    let settings = thread::scope(|scope| {
        let get = scope.spawn(|| ims.get_services());
        let request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_service_request(&request, SERVICE_IMS, 0x07, IMS_GET_SERVICES);
        responses
            .send(response(
                &request,
                &[
                    Tlv::new(0x02, &[0, 0, 0, 0]),
                    Tlv::new(0x11, &[1]),
                    Tlv::new(0x15, &[0]),
                    Tlv::new(0x18, &[1]),
                ],
            ))
            .unwrap();
        get.join().unwrap().unwrap()
    });
    assert_eq!(settings.voice_over_lte, Some(true));
    assert_eq!(settings.voice_wifi, Some(false));
    assert_eq!(settings.ims, Some(true));

    thread::scope(|scope| {
        let set = scope.spawn(|| {
            ims.set_services(ImsSettingsUpdate {
                voice_over_lte: Some(false),
                voice_wifi: Some(true),
                call_mode: Some(3),
                sms: Some(true),
                ..ImsSettingsUpdate::default()
            })
        });
        let request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_service_request(&request, SERVICE_IMS, 0x07, IMS_SET_SERVICES);
        let packet = decode_packet(&request).unwrap();
        let actual: Vec<_> = packet
            .tlvs()
            .map(|tlv| (tlv.kind, tlv.value.to_vec()))
            .collect();
        assert_eq!(
            actual,
            [
                (0x10, vec![0]),
                (0x14, vec![1]),
                (0x15, vec![3]),
                (0x1a, vec![1]),
            ]
        );
        responses
            .send(response(&request, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
            .unwrap();
        set.join().unwrap().unwrap();
    });

    let registration = thread::scope(|scope| {
        let get = scope.spawn(|| ims.get_registration_status());
        let request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_service_request(&request, SERVICE_IMSA, 0x09, IMSA_GET_REGISTRATION);
        responses
            .send(response(
                &request,
                &[
                    Tlv::new(0x02, &[0, 0, 0, 0]),
                    Tlv::new(0x11, &[0x34, 0x12]),
                    Tlv::new(0x12, &[2, 0, 0, 0]),
                    Tlv::new(0x13, b"registered"),
                    Tlv::new(0x14, &[0, 0, 0, 0]),
                ],
            ))
            .unwrap();
        get.join().unwrap().unwrap()
    });
    assert_eq!(registration.state, 2);
    assert_eq!(registration.error_code, Some(0x1234));
    assert_eq!(registration.error_message.as_deref(), Some("registered"));
    assert_eq!(registration.technology, Some(0));

    let close_client = ims.clone();
    let closing = thread::spawn(move || close_client.close());
    let imsa_release = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_release(&imsa_release, SERVICE_IMSA, 0x09);
    responses
        .send(response(&imsa_release, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();

    let ims_release = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_release(&ims_release, SERVICE_IMS, 0x07);
    responses
        .send(response(&ims_release, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();
    closing.join().unwrap().unwrap();
    assert!(ims.get_services().is_err());
    client.close().unwrap();
}

#[test]
fn failed_second_service_allocation_releases_first_client_id() {
    let (client, requests, responses) = fake_client();
    let open_client = client.clone();
    let opening = thread::spawn(move || ImsClient::open(&open_client));

    let ims_allocate = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_allocate(&ims_allocate, SERVICE_IMS);
    responses
        .send(response(
            &ims_allocate,
            &[Tlv::new(0x02, &[0, 0, 0, 0]), Tlv::new(0x01, &[0x12, 0x07])],
        ))
        .unwrap();

    let imsa_allocate = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_allocate(&imsa_allocate, SERVICE_IMSA);
    responses
        .send(response(
            &imsa_allocate,
            &[
                Tlv::new(0x02, &[1, 0, 0x47, 0]),
                Tlv::new(0x01, &[0x21, 0x09]),
            ],
        ))
        .unwrap();

    let ims_release = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_release(&ims_release, SERVICE_IMS, 0x07);
    responses
        .send(response(&ims_release, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();
    assert!(opening.join().unwrap().is_err());
    client.close().unwrap();
}

#[test]
fn nas_client_reads_go_compatible_operator_name_and_releases_its_id() {
    let (client, requests, responses) = fake_client();
    let open_client = client.clone();
    let opening = thread::spawn(move || NasClient::open(&open_client));

    let allocate = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_allocate(&allocate, SERVICE_NAS);
    responses
        .send(response(
            &allocate,
            &[Tlv::new(0x02, &[0, 0, 0, 0]), Tlv::new(0x01, &[0x03, 0x04])],
        ))
        .unwrap();
    let nas = opening.join().unwrap().unwrap();

    let operator = thread::scope(|scope| {
        let query = scope.spawn(|| nas.get_operator_name());
        let request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_service_request(&request, SERVICE_NAS, 0x04, NAS_GET_OPERATOR_NAME);
        responses
            .send(response(
                &request,
                &[
                    Tlv::new(0x02, &[0, 0, 0, 0]),
                    Tlv::new(0x10, &[8, b'V', b'o', b'd', b'a', b'f', b'o', b'n', b'e']),
                    Tlv::new(0x13, b"Vodafone UK"),
                ],
            ))
            .unwrap();
        query.join().unwrap().unwrap()
    });
    assert_eq!(operator.service_provider_name, "Vodafone");
    assert_eq!(operator.operator_string_name, "Vodafone UK");
    assert_eq!(operator.preferred_name(), "Vodafone");

    let closed = thread::scope(|scope| {
        let close = scope.spawn(|| nas.close());
        let release = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_ctl_release(&release, SERVICE_NAS, 0x04);
        responses
            .send(response(&release, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
            .unwrap();
        close.join().unwrap()
    });
    closed.unwrap();
    client.close().unwrap();
}

#[test]
fn uim_client_prefers_first_exact_home_plmn_from_opl_records() {
    let (client, requests, responses) = fake_client();
    let open_client = client.clone();
    let opening = thread::spawn(move || UimClient::open(&open_client));

    let allocate = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_allocate(&allocate, SERVICE_UIM);
    responses
        .send(response(
            &allocate,
            &[
                Tlv::new(0x02, &[0, 0, 0, 0]),
                Tlv::new(0x01, &[SERVICE_UIM as u8, 0x0b]),
            ],
        ))
        .unwrap();
    let uim = opening.join().unwrap().unwrap();

    let native = thread::scope(|scope| {
        let lookup = scope.spawn(|| uim.get_native_mcc_mnc());

        let attributes = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_service_request(&attributes, SERVICE_UIM, 0x0b, UIM_GET_FILE_ATTRIBUTES);
        let packet = decode_packet(&attributes).unwrap();
        assert_eq!(packet.find_tlv(0x01).unwrap().value, [0, 0]);
        assert_eq!(
            packet.find_tlv(0x02).unwrap().value,
            [0xc6, 0x6f, 4, 0, 0x3f, 0xff, 0x7f]
        );
        let mut file = [0_u8; 26];
        file[..2].copy_from_slice(&16_u16.to_le_bytes());
        file[2..4].copy_from_slice(&0x6fc6_u16.to_le_bytes());
        file[4] = 1;
        file[5..7].copy_from_slice(&8_u16.to_le_bytes());
        file[7..9].copy_from_slice(&2_u16.to_le_bytes());
        responses
            .send(response(
                &attributes,
                &[Tlv::new(0x02, &[0, 0, 0, 0]), Tlv::new(0x11, &file)],
            ))
            .unwrap();

        let first = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_service_request(&first, SERVICE_UIM, 0x0b, UIM_READ_RECORD);
        assert_record_request(&first, 1);
        let invalid_wildcard = [0x13, 0x0a, 0x62, 0, 0, 0, 0, 1];
        responses
            .send(response(
                &first,
                &[
                    Tlv::new(0x02, &[0, 0, 0, 0]),
                    Tlv::new(0x11, &prefixed(&invalid_wildcard)),
                ],
            ))
            .unwrap();

        let second = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_service_request(&second, SERVICE_UIM, 0x0b, UIM_READ_RECORD);
        assert_record_request(&second, 2);
        let valid_plmn = [0x13, 0x00, 0x62, 0, 0, 0, 0, 1];
        responses
            .send(response(
                &second,
                &[
                    Tlv::new(0x02, &[0, 0, 0, 0]),
                    Tlv::new(0x11, &prefixed(&valid_plmn)),
                ],
            ))
            .unwrap();
        lookup.join().unwrap().unwrap()
    });
    assert_eq!((native.mcc(), native.mnc()), ("310", "260"));

    let close_client = uim.clone();
    let closing = thread::spawn(move || close_client.close());
    let release = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_release(&release, SERVICE_UIM, 0x0b);
    responses
        .send(response(&release, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();
    closing.join().unwrap().unwrap();
    client.close().unwrap();
}

#[test]
fn uim_client_falls_back_from_unreadable_opl_to_imsi_and_ef_ad() {
    let (client, requests, responses) = fake_client();
    let open_client = client.clone();
    let opening = thread::spawn(move || UimClient::open(&open_client));
    let allocate = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_allocate(&allocate, SERVICE_UIM);
    responses
        .send(response(
            &allocate,
            &[
                Tlv::new(0x02, &[0, 0, 0, 0]),
                Tlv::new(0x01, &[SERVICE_UIM as u8, 0x0c]),
            ],
        ))
        .unwrap();
    let uim = opening.join().unwrap().unwrap();

    let native = thread::scope(|scope| {
        let lookup = scope.spawn(|| uim.get_native_mcc_mnc());
        let paths: [&[u8]; 3] = [&[0, 0x3f, 0xff, 0x7f], &[0x20, 0x7f], &[]];

        for path in paths {
            let attributes = requests.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_service_request(&attributes, SERVICE_UIM, 0x0c, UIM_GET_FILE_ATTRIBUTES);
            assert_uim_file(&attributes, 0x6fc6, path);
            responses
                .send(response(&attributes, &[Tlv::new(0x02, &[1, 0, 0x47, 0])]))
                .unwrap();
        }

        for path in paths {
            let record = requests.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_service_request(&record, SERVICE_UIM, 0x0c, UIM_READ_RECORD);
            assert_uim_file(&record, 0x6fc6, path);
            assert_eq!(
                decode_packet(&record)
                    .unwrap()
                    .find_tlv(0x03)
                    .unwrap()
                    .value,
                [1, 0, 8, 0]
            );
            responses
                .send(response(&record, &[Tlv::new(0x02, &[1, 0, 0x47, 0])]))
                .unwrap();
        }

        let imsi_request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_service_request(&imsi_request, SERVICE_UIM, 0x0c, UIM_READ_TRANSPARENT);
        assert_uim_file(&imsi_request, 0x6f07, paths[0]);
        let imsi_file = [15, 0x31, 0x01, 0x62, 0x10, 0x32, 0x54, 0x76, 0x98];
        responses
            .send(response(
                &imsi_request,
                &[
                    Tlv::new(0x02, &[0, 0, 0, 0]),
                    Tlv::new(0x11, &prefixed(&imsi_file)),
                ],
            ))
            .unwrap();

        let ad_request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_service_request(&ad_request, SERVICE_UIM, 0x0c, UIM_READ_TRANSPARENT);
        assert_uim_file(&ad_request, 0x6fad, paths[0]);
        responses
            .send(response(
                &ad_request,
                &[
                    Tlv::new(0x02, &[0, 0, 0, 0]),
                    Tlv::new(0x11, &prefixed(&[0, 0, 0, 3])),
                ],
            ))
            .unwrap();
        lookup.join().unwrap().unwrap()
    });
    assert_eq!((native.mcc(), native.mnc()), ("310", "260"));

    let close_client = uim.clone();
    let closing = thread::spawn(move || close_client.close());
    let release = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ctl_release(&release, SERVICE_UIM, 0x0c);
    responses
        .send(response(&release, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();
    closing.join().unwrap().unwrap();
    client.close().unwrap();
}

fn assert_record_request(request: &[u8], record_number: u16) {
    let packet = decode_packet(request).unwrap();
    assert_eq!(
        packet.find_tlv(0x02).unwrap().value,
        [0xc6, 0x6f, 4, 0, 0x3f, 0xff, 0x7f]
    );
    assert_eq!(
        packet.find_tlv(0x03).unwrap().value,
        [record_number as u8, 0, 8, 0]
    );
}

fn assert_uim_file(request: &[u8], file_id: u16, path: &[u8]) {
    let mut expected = Vec::with_capacity(path.len() + 3);
    expected.extend_from_slice(&file_id.to_le_bytes());
    expected.push(path.len() as u8);
    expected.extend_from_slice(path);
    assert_eq!(
        decode_packet(request)
            .unwrap()
            .find_tlv(0x02)
            .unwrap()
            .value,
        expected
    );
}

fn prefixed(value: &[u8]) -> Vec<u8> {
    let mut prefixed = Vec::with_capacity(value.len() + 2);
    prefixed.extend_from_slice(&(value.len() as u16).to_le_bytes());
    prefixed.extend_from_slice(value);
    prefixed
}

fn assert_ctl_allocate(request: &[u8], service: u16) {
    let packet = decode_packet(request).unwrap();
    assert_eq!(packet.meta.service, 0);
    assert_eq!(packet.meta.message_id, 0x0022);
    assert_eq!(packet.find_tlv(0x01).unwrap().value, [service as u8]);
}

fn assert_ctl_release(request: &[u8], service: u16, client_id: u8) {
    let packet = decode_packet(request).unwrap();
    assert_eq!(packet.meta.service, 0);
    assert_eq!(packet.meta.message_id, 0x0023);
    assert_eq!(
        packet.find_tlv(0x01).unwrap().value,
        [service as u8, client_id]
    );
}

fn assert_service_request(request: &[u8], service: u16, client_id: u8, message: u16) {
    let packet = decode_packet(request).unwrap();
    assert_eq!(packet.meta.service, service);
    assert_eq!(packet.meta.client_id, client_id);
    assert_eq!(packet.meta.message_id, message);
}

fn fake_client() -> (Client, Receiver<Vec<u8>>, Sender<Vec<u8>>) {
    let (outgoing, requests) = mpsc::sync_channel(16);
    let (responses, incoming) = mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let transport = MemoryTransport {
        incoming: Mutex::new(incoming),
        incoming_wake: responses.clone(),
        outgoing,
        closed,
    };
    let client = Client::from_transport(
        Box::new(transport),
        ClientOptions {
            default_request_timeout: Duration::from_secs(1),
            ..ClientOptions::default()
        },
    )
    .unwrap();
    (client, requests, responses)
}

fn response(request: &[u8], tlvs: &[Tlv<'_>]) -> Vec<u8> {
    let request = decode_packet(request).unwrap();
    let mut meta = PacketMeta::request(
        request.meta.service,
        request.meta.client_id,
        request.meta.transaction_id,
        request.meta.message_id,
    );
    meta.message_control_flags = if meta.service == 0 { 0x01 } else { 0x02 };
    let mut frame = vec![0; 256];
    let length = encode_packet(meta, tlvs, &mut frame).unwrap();
    frame.truncate(length);
    frame
}
