use super::*;
use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

#[test]
fn private_route_start_requires_operation_credentials_and_other_adb_routes_reject_them() {
    // The radio owner validates the exact token and current reservation. The
    // HTTP boundary requires both fields only for the self-test start action.
    for body in [
        r#"{"producer_id":"producer-1"}"#,
        r#"{"producer_id":"producer-1","call_mode_token":"token-1"}"#,
        r#"{"producer_id":"producer-1","call_mode_sequence":1}"#,
        r#"{"producer_id":"producer-1","call_mode_token":"","call_mode_sequence":1}"#,
        r#"{"producer_id":"producer-1","call_mode_token":"token-1","call_mode_sequence":0}"#,
    ] {
        let input: ProbeRequest = serde_json::from_str(body).unwrap();
        assert_eq!(
            call_mode_credentials(&input, true).unwrap_err().code,
            "module_voice_operation_invalid"
        );
    }
    let input: ProbeRequest = serde_json::from_str(
        r#"{"producer_id":"producer-1","call_mode_token":"token-1","call_mode_sequence":7}"#,
    )
    .unwrap();
    assert_eq!(
        call_mode_credentials(&input, true).unwrap(),
        Some(("token-1", 7))
    );
    assert!(call_mode_credentials(&input, false).is_err());
    let input: ProbeRequest = serde_json::from_str(r#"{"producer_id":"producer-1"}"#).unwrap();
    assert_eq!(call_mode_credentials(&input, false).unwrap(), None);
    assert!(
        serde_json::from_str::<ProbeRequest>(r#"{"producer_id":"producer-1","skip_lock":true}"#,)
            .is_err()
    );
}

struct FakeUsb {
    incoming: VecDeque<u8>,
    outgoing: Vec<u8>,
    max_chunk: usize,
}

impl FakeUsb {
    fn new(incoming: Vec<u8>, max_chunk: usize) -> Self {
        Self {
            incoming: incoming.into(),
            outgoing: Vec::new(),
            max_chunk,
        }
    }
}

impl BulkIo for FakeUsb {
    fn read_bulk(&mut self, buffer: &mut [u8], _timeout: Duration) -> Result<usize, ProbeError> {
        if self.incoming.is_empty() {
            return Err(ProbeError::new(
                "module_adb_usb_timeout",
                "fixture exhausted",
            ));
        }
        let length = buffer.len().min(self.max_chunk).min(self.incoming.len());
        for byte in &mut buffer[..length] {
            *byte = self.incoming.pop_front().expect("length bounded by queue");
        }
        Ok(length)
    }

    fn write_bulk(&mut self, payload: &[u8], _timeout: Duration) -> Result<usize, ProbeError> {
        self.outgoing.extend_from_slice(payload);
        Ok(payload.len())
    }
}

struct FakeModuleVoiceADB {
    results: VecDeque<(Vec<u8>, u32)>,
    commands: Vec<String>,
    timeouts: Vec<Duration>,
}

impl FakeModuleVoiceADB {
    fn new(statuses: &[u32]) -> Self {
        Self {
            results: statuses
                .iter()
                .map(|status| (Vec::new(), *status))
                .collect(),
            commands: Vec::new(),
            timeouts: Vec::new(),
        }
    }

    fn with_outputs(results: &[(&str, u32)]) -> Self {
        Self {
            results: results
                .iter()
                .map(|(output, status)| (output.as_bytes().to_vec(), *status))
                .collect(),
            commands: Vec::new(),
            timeouts: Vec::new(),
        }
    }
}

impl ModuleVoiceADB for FakeModuleVoiceADB {
    fn shell_checked_timeout(
        &mut self,
        command: &str,
        timeout: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        self.commands.push(command.to_owned());
        self.timeouts.push(timeout);
        self.results.pop_front().ok_or_else(|| {
            ProbeError::new("fixture_exhausted", "module voice ADB fixture exhausted")
        })
    }
}

fn frame(tag: &[u8; 4], arg0: u32, arg1: u32, payload: &[u8]) -> Vec<u8> {
    let header = encode_header(command(tag), arg0, arg1, payload).unwrap();
    [header.as_slice(), payload].concat()
}

fn successful_exchange(shell_output: &[u8]) -> Vec<u8> {
    [
        frame(
            b"CNXN",
            ADB_VERSION,
            MAX_PAYLOAD as u32,
            b"device::Switch\0",
        ),
        frame(b"OKAY", 42, 1, b""),
        frame(b"WRTE", 42, 1, shell_output),
        frame(b"CLSE", 42, 1, b""),
    ]
    .concat()
}

fn root_output(token: &str) -> Vec<u8> {
    format!("0\n\n__SWITCH_ADB_STATUS_{token}_0__\n").into_bytes()
}

fn sync_push_exchange(data_length: usize, max_payload: usize, fail: Option<&[u8]>) -> Vec<u8> {
    let mut incoming = frame(
        b"CNXN",
        ADB_VERSION,
        max_payload as u32,
        b"device::Switch\0",
    );
    incoming.extend(frame(b"OKAY", 42, 1, b""));
    let chunk_capacity = max_payload - 8;
    let packet_count = 1 + data_length.div_ceil(chunk_capacity) + 1;
    for _ in 0..packet_count {
        incoming.extend(frame(b"OKAY", 42, 1, b""));
    }
    let response = if let Some(message) = fail {
        let mut response = b"FAIL".to_vec();
        response.extend((message.len() as u32).to_le_bytes());
        response.extend(message);
        response
    } else {
        [b"OKAY".as_slice(), &0u32.to_le_bytes()].concat()
    };
    incoming.extend(frame(b"WRTE", 42, 1, &response));
    incoming.extend(frame(b"CLSE", 42, 1, b""));
    incoming
}

fn outgoing_frames(bytes: &[u8]) -> Vec<(u32, u32, u32, Vec<u8>)> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let header = &bytes[offset..offset + 24];
        let (command, arg0, arg1, size, _, _) = decode_header(header).unwrap();
        offset += 24;
        let end = offset + size as usize;
        frames.push((command, arg0, arg1, bytes[offset..end].to_vec()));
        offset = end;
    }
    frames
}

#[test]
fn read_only_root_probe_uses_shell_id_u_and_accepts_fragmented_packets() {
    let token = "fixture-token";
    let mut usb = FakeUsb::new(successful_exchange(&root_output(token)), 3);
    {
        AdbProbe::new(&mut usb)
            .run_with_token(token)
            .expect("root probe fixture should pass");
    }
    assert!(
        usb.outgoing
            .windows(b"shell:{ id -u;".len())
            .any(|window| window == b"shell:{ id -u;")
    );
    assert!(
        usb.outgoing
            .windows(b"__SWITCH_ADB_STATUS_fixture-token_".len())
            .any(|window| { window == b"__SWITCH_ADB_STATUS_fixture-token_" })
    );
}

#[test]
fn module_voice_prepare_checks_alsa_calibration_interfaces_and_helper_in_order() {
    let mut adb = FakeModuleVoiceADB::new(&[0, 0, 0, 0, 0]);
    prepare_module_voice_audio(&mut adb).expect("ready module should pass preparation");
    assert_eq!(adb.commands.len(), 5);
    assert_eq!(adb.commands[0], module_voice_sound_device_checks());
    assert!(adb.commands[1].contains("/dev/snd/pcmC0D4p"));
    assert!(adb.commands[2].contains("ACDB -> Sent VocProc Cal!"));
    assert!(adb.commands[2].contains("/run/proletarius-alsaucm.pid"));
    assert_eq!(
        adb.commands[3],
        "test -c /dev/ttyGS0 && test -p /run/voc_svr"
    );
    assert_eq!(
        adb.commands[4],
        "'/tmp/proletarius-call/mavo-pcm-bridge.armv7' --check"
    );
    assert_eq!(adb.timeouts, [8, 25, 25, 8, 15].map(Duration::from_secs));
}

#[test]
fn module_voice_prepare_loads_only_missing_drivers_without_hot_unload() {
    let mut adb = FakeModuleVoiceADB::new(&[1, 1, 1, 0, 0, 0, 0, 0, 0]);
    prepare_module_voice_audio(&mut adb).expect("missing driver should be loaded then verified");
    assert!(
        adb.commands
            .iter()
            .any(|command| { command == "insmod '/tmp/proletarius-call/qdc507_aprv3.ko'" })
    );
    assert!(
        !adb.commands
            .iter()
            .any(|command| { command == "insmod '/tmp/proletarius-call/qdc507_voice.ko'" })
    );
    assert!(!adb.commands.iter().any(|command| command.contains("rmmod")));
}

#[test]
fn module_voice_prepare_refuses_legacy_driver_and_stops_before_mutation() {
    let mut adb = FakeModuleVoiceADB::new(&[1, 0]);
    let error = prepare_module_voice_audio(&mut adb).unwrap_err();
    assert_eq!(error.code, "module_voice_legacy_driver_loaded");
    assert!(error.to_string().contains("请重启模块"));
    assert_eq!(adb.commands.len(), 2);
    assert!(
        !adb.commands
            .iter()
            .any(|command| command.starts_with("insmod"))
    );
}

#[test]
fn module_voice_prepare_preserves_calibration_diagnostics_and_failure_stage() {
    let mut adb = FakeModuleVoiceADB::with_outputs(&[
        ("", 0),
        ("", 0),
        ("", 1),
        ("ACDB service exited before calibration", 0),
    ]);
    let error = prepare_module_voice_audio(&mut adb).unwrap_err();
    assert_eq!(error.code, "module_voice_calibration_failed");
    assert!(error.to_string().contains("ACDB service exited"));
    assert_eq!(adb.commands.len(), 4);
}

#[test]
fn voice_runtime_readiness_is_bound_to_producer_generation_and_module_boot() {
    let boot_a = "00112233-4455-6677-8899-aabbccddeeff";
    let boot_b = "10112233-4455-6677-8899-aabbccddeeff";
    let mut readiness = VoiceRuntimeReadiness::default();

    assert_eq!(readiness.advance_next("producer-a").unwrap(), 1);
    readiness.validate_token("producer-a", 1).unwrap();
    assert!(!readiness.is_prepared("producer-a", 1, boot_a));
    readiness
        .mark_prepared("producer-a", 1, boot_a.to_owned())
        .unwrap();
    assert!(readiness.is_prepared("producer-a", 1, boot_a));

    // A module reboot invalidates the Rust cache even if a detach notification
    // was lost between the Go producer and the daemon.
    assert!(!readiness.is_prepared("producer-a", 1, boot_b));
    assert!(!readiness.is_prepared("producer-a", 1, boot_a));

    assert_eq!(readiness.advance_next("producer-a").unwrap(), 2);
    assert!(!readiness.is_prepared("producer-a", 1, boot_a));
    assert!(readiness.validate_token("producer-a", 1).is_err());
    assert!(
        readiness
            .mark_prepared("producer-a", 1, boot_a.to_owned())
            .is_err()
    );

    // A new supervised Go producer starts a fresh local runtime generation.
    assert_eq!(readiness.advance_next("producer-b").unwrap(), 1);
    assert!(!readiness.is_prepared("producer-b", 1, boot_a));
    assert!(
        readiness
            .mark_prepared("producer-a", 2, boot_a.to_owned())
            .is_err()
    );
}

#[test]
fn rust_assigns_runtime_generations_and_invalidates_prior_preparation() {
    let boot_id = "00112233-4455-6677-8899-aabbccddeeff";
    let mut readiness = VoiceRuntimeReadiness::default();

    assert_eq!(readiness.advance_next("producer-a").unwrap(), 1);
    readiness
        .mark_prepared("producer-a", 1, boot_id.to_owned())
        .unwrap();
    assert!(readiness.is_prepared("producer-a", 1, boot_id));

    assert_eq!(readiness.advance_next("producer-a").unwrap(), 2);
    assert!(!readiness.is_prepared("producer-a", 1, boot_id));
    assert!(readiness.validate_token("producer-a", 1).is_err());
    assert_eq!(
        readiness.validate_token("producer-a", 3).unwrap_err().code,
        "unissued_module_voice_runtime_generation"
    );

    // A replacement supervised producer starts a fresh fence.
    assert_eq!(readiness.advance_next("producer-b").unwrap(), 1);
    assert!(!readiness.is_prepared("producer-a", 2, boot_id));

    readiness.generation = u64::MAX;
    assert_eq!(
        readiness.advance_next("producer-b").unwrap_err().code,
        "module_voice_runtime_generation_exhausted"
    );
}

#[test]
fn module_boot_id_read_rejects_malformed_or_failed_shell_results() {
    let boot_id = "00112233-4455-6677-8899-AABBCCDDEEFF";
    let mut valid = FakeModuleVoiceADB::with_outputs(&[(boot_id, 0)]);
    assert_eq!(
        read_module_boot_id(&mut valid).unwrap(),
        boot_id.to_ascii_lowercase()
    );
    assert_eq!(valid.commands, ["cat /proc/sys/kernel/random/boot_id"]);

    let mut malformed = FakeModuleVoiceADB::with_outputs(&[("not-a-boot-id", 0)]);
    assert_eq!(
        read_module_boot_id(&mut malformed).unwrap_err().code,
        "module_voice_boot_id_unconfirmed"
    );
    let mut failed = FakeModuleVoiceADB::with_outputs(&[(boot_id, 1)]);
    assert_eq!(
        read_module_boot_id(&mut failed).unwrap_err().code,
        "module_voice_boot_id_unconfirmed"
    );
}

#[test]
fn module_voice_prepare_reports_driver_load_diagnostics_without_advancing() {
    let mut adb = FakeModuleVoiceADB::with_outputs(&[
        ("", 1),
        ("", 1),
        ("", 1),
        ("insmod: invalid module format", 1),
        ("qdc507_aprv3: disagrees about version of symbol", 0),
    ]);
    let error = prepare_module_voice_audio(&mut adb).unwrap_err();
    assert_eq!(error.code, "module_voice_driver_load_failed");
    assert!(error.to_string().contains("invalid module format"));
    assert!(
        error
            .to_string()
            .contains("disagrees about version of symbol")
    );
    assert_eq!(adb.commands.len(), 5);
    assert!(adb.commands[4].starts_with("dmesg | tail -c "));
}

#[test]
fn module_voice_route_status_distinguishes_active_stopped_and_unknown() {
    let mut active = FakeModuleVoiceADB::new(&[0]);
    assert!(module_voice_route_status(&mut active).unwrap());
    assert_eq!(active.commands.len(), 1);

    let mut stopped = FakeModuleVoiceADB::new(&[1, 0]);
    assert!(!module_voice_route_status(&mut stopped).unwrap());
    assert_eq!(stopped.commands.len(), 2);

    let mut unknown = FakeModuleVoiceADB::new(&[1, 1]);
    let error = module_voice_route_status(&mut unknown).unwrap_err();
    assert_eq!(error.code, "module_voice_route_state_unconfirmed");
}

#[test]
fn module_voice_route_start_is_idempotent_and_confirms_after_helper_launch() {
    let mut already_active = FakeModuleVoiceADB::new(&[0]);
    let mut current = || Ok(());
    start_module_voice_route(
        &mut already_active,
        Instant::now() + Duration::from_secs(1),
        Duration::ZERO,
        &mut current,
    )
    .unwrap();
    assert_eq!(already_active.commands.len(), 1);

    let mut starts = FakeModuleVoiceADB::new(&[1, 0, 0]);
    let mut current = || Ok(());
    start_module_voice_route(
        &mut starts,
        Instant::now() + Duration::from_secs(1),
        Duration::ZERO,
        &mut current,
    )
    .unwrap();
    assert!(starts.commands[1].contains("nohup '/tmp/proletarius-call/mavo-pcm-bridge.armv7'"));
    assert!(starts.commands[1].contains("--voice-route-session --verbose"));
    assert!(starts.commands[2].contains("VoLTE route session active on hw:0,4"));
}

#[test]
fn module_voice_route_start_accepts_lost_launch_reply_only_after_ready_confirmation() {
    let mut adb = FakeModuleVoiceADB::with_outputs(&[
        ("", 1),
        ("ADB shell reply lost during USB re-enumeration", 1),
        ("", 0),
    ]);
    let mut current = || Ok(());
    start_module_voice_route(
        &mut adb,
        Instant::now() + Duration::from_secs(1),
        Duration::ZERO,
        &mut current,
    )
    .unwrap();
    assert_eq!(adb.commands.len(), 3);
    assert!(adb.commands[1].contains("nohup"));
    assert!(adb.commands[2].contains("VoLTE route session active on hw:0,4"));
}

#[test]
fn module_voice_route_start_rolls_back_helper_and_mixer_when_readiness_times_out() {
    let mut adb = FakeModuleVoiceADB::with_outputs(&[
        ("", 1),
        ("helper launch was not acknowledged", 1),
        ("route never became ready", 0),
        ("", 0),
        ("", 0),
        ("", 0),
    ]);
    let mut current = || Ok(());
    let error = start_module_voice_route(
        &mut adb,
        Instant::now() - Duration::from_millis(1),
        Duration::ZERO,
        &mut current,
    )
    .unwrap_err();
    assert_eq!(error.code, "module_voice_route_start_failed");
    assert!(
        error
            .to_string()
            .contains("helper launch was not acknowledged")
    );
    assert!(adb.commands[3].contains("kill -TERM"));
    assert!(!adb.commands[3].contains("kill -KILL"));
    assert!(adb.commands[5].contains("audio_enable"));
    assert!(adb.commands[5].contains("printf 'T\\n'"));
}

#[test]
fn module_voice_route_stop_uses_pid_identity_and_confirms_mixer_rollback() {
    let mut adb = FakeModuleVoiceADB::new(&[0, 0, 0]);
    let mut current = || Ok(());
    stop_module_voice_route(&mut adb, 2, Duration::ZERO, 2, Duration::ZERO, &mut current).unwrap();
    assert!(adb.commands[0].contains("current_start"));
    assert!(adb.commands[0].contains("/tmp/proletarius-call/mavo-pcm-bridge.armv7"));
    assert!(adb.commands[0].contains("^--voice-route-session$"));
    assert!(adb.commands[0].contains("kill -TERM"));
    assert!(!adb.commands[0].contains("kill -KILL"));
    assert!(adb.commands[2].contains("/sys/class/android_usb/f_audio/audio_enable"));
    assert!(adb.commands[2].contains("/run/voc_svr"));
}

#[test]
fn module_voice_route_stop_does_not_rollback_mixer_until_helper_exit_is_confirmed() {
    let mut adb = FakeModuleVoiceADB::new(&[0, 1, 1]);
    let mut current = || Ok(());
    let error =
        stop_module_voice_route(&mut adb, 2, Duration::ZERO, 2, Duration::ZERO, &mut current)
            .unwrap_err();
    assert_eq!(error.code, "module_voice_route_stop_unconfirmed");
    assert_eq!(adb.commands.len(), 3);
    assert!(
        !adb.commands
            .iter()
            .any(|command| command.contains("audio_enable"))
    );
    assert!(adb.commands[0].contains("kill -TERM"));
    assert!(!adb.commands[0].contains("kill -KILL"));
}

#[test]
fn module_voice_route_stop_reports_unconfirmed_mixer_rollback_after_retries() {
    let mut adb = FakeModuleVoiceADB::new(&[0, 0, 1, 1]);
    let mut current = || Ok(());
    let error =
        stop_module_voice_route(&mut adb, 1, Duration::ZERO, 2, Duration::ZERO, &mut current)
            .unwrap_err();
    assert_eq!(error.code, "module_voice_route_cleanup_unconfirmed");
    assert_eq!(adb.commands.len(), 4);
    assert!(adb.commands[2].contains("printf 'T\\n'"));
    assert!(adb.commands[3].contains("printf 'B\\n'"));
}

#[test]
fn module_voice_route_start_does_not_launch_after_generation_changes() {
    let mut adb = FakeModuleVoiceADB::new(&[1]);
    let mut checks = 0;
    let mut current = || {
        checks += 1;
        Err(ProbeError::new(
            "stale_backend_generation",
            "producer changed",
        ))
    };
    let error = start_module_voice_route(
        &mut adb,
        Instant::now() + Duration::from_secs(1),
        Duration::ZERO,
        &mut current,
    )
    .unwrap_err();
    assert_eq!(error.code, "stale_backend_generation");
    assert_eq!(checks, 1);
    assert_eq!(adb.commands.len(), 1);
}

#[test]
fn native_sync_push_transfers_exact_bytes_mode_and_done_timestamp() {
    let data: Vec<u8> = (0..130).map(|value| value as u8).collect();
    let mut usb = FakeUsb::new(sync_push_exchange(data.len(), 64, None), 3);
    let mut adb = AdbProbe::new(&mut usb);
    adb.connect().unwrap();
    adb.push_with_mtime(
        &data,
        "/tmp/proletarius-call/qdc507_voice.ko",
        0o100755,
        0x12345678,
    )
    .unwrap();

    let frames = outgoing_frames(&usb.outgoing);
    assert_eq!(frames.iter().filter(|frame| frame.0 == ADB_CNXN).count(), 1);
    let stream_opens: Vec<_> = frames.iter().filter(|frame| frame.0 == ADB_OPEN).collect();
    assert_eq!(stream_opens.len(), 1);
    assert_eq!(stream_opens[0].3, b"sync:\0");

    let sync_packets: Vec<_> = frames
        .iter()
        .filter(|frame| frame.0 == ADB_WRTE)
        .map(|frame| frame.3.as_slice())
        .collect();
    assert_eq!(
        sync_packets[0],
        b"SEND\x2b\0\0\0/tmp/proletarius-call/qdc507_voice.ko,33261"
    );

    let mut transferred = Vec::new();
    for packet in &sync_packets[1..sync_packets.len() - 1] {
        assert_eq!(&packet[..4], b"DATA");
        let length = u32::from_le_bytes(packet[4..8].try_into().unwrap()) as usize;
        assert_eq!(packet.len(), 8 + length);
        transferred.extend_from_slice(&packet[8..]);
    }
    assert_eq!(transferred, data);
    let done = sync_packets.last().unwrap();
    assert_eq!(done.len(), 8);
    assert_eq!(&done[..4], b"DONE");
    assert_eq!(
        u32::from_le_bytes(done[4..8].try_into().unwrap()),
        0x12345678
    );
    assert!(frames.iter().any(|frame| frame.0 == ADB_CLSE));
}

#[test]
fn native_sync_push_rejects_invalid_targets_and_server_failures() {
    let mut empty = FakeUsb::new(Vec::new(), 64);
    let error = AdbProbe::new(&mut empty)
        .push_with_mtime(b"data", "/tmp/bad,path", 0o100644, 1)
        .unwrap_err();
    assert_eq!(error.code, "module_adb_protocol_invalid");
    assert!(empty.outgoing.is_empty());

    let detail = b"permission denied";
    let mut usb = FakeUsb::new(sync_push_exchange(4, 64, Some(detail)), 5);
    let mut adb = AdbProbe::new(&mut usb);
    adb.connect().unwrap();
    let error = adb
        .push_with_mtime(b"data", "/tmp/voice", 0o100644, 1)
        .unwrap_err();
    assert_eq!(error.code, "module_adb_file_transfer_failed");
    assert!(error.to_string().contains("permission denied"));
}

#[test]
fn native_sync_push_fails_closed_on_wrong_stream_acknowledgement() {
    let incoming = [
        frame(b"CNXN", ADB_VERSION, 64, b"device::Switch\0"),
        frame(b"OKAY", 42, 1, b""),
        frame(b"OKAY", 99, 1, b""),
    ]
    .concat();
    let mut usb = FakeUsb::new(incoming, 32);
    let mut adb = AdbProbe::new(&mut usb);
    adb.connect().unwrap();
    let error = adb
        .push_with_mtime(b"data", "/tmp/voice", 0o100644, 1)
        .unwrap_err();
    assert_eq!(error.code, "module_adb_protocol_invalid");
}

#[test]
fn root_probe_rejects_non_root_and_nonzero_shell_status() {
    let token = "fixture-token";
    let not_root = format!("2000\n\n__SWITCH_ADB_STATUS_{token}_0__\n").into_bytes();
    let mut usb = FakeUsb::new(successful_exchange(&not_root), 24);
    let error = AdbProbe::new(&mut usb).run_with_token(token).unwrap_err();
    assert_eq!(error.code, "module_adb_root_required");

    let failed_command = format!("0\n\n__SWITCH_ADB_STATUS_{token}_1__\n").into_bytes();
    let mut usb = FakeUsb::new(successful_exchange(&failed_command), 24);
    let error = AdbProbe::new(&mut usb).run_with_token(token).unwrap_err();
    assert_eq!(error.code, "module_adb_root_required");
}

#[test]
fn root_probe_rejects_authentication_and_corrupt_frames() {
    let mut usb = FakeUsb::new(frame(b"AUTH", 1, 0, b""), 24);
    let error = AdbProbe::new(&mut usb).connect().unwrap_err();
    assert_eq!(error.code, "module_adb_authentication_required");

    let mut corrupt = frame(
        b"CNXN",
        ADB_VERSION,
        MAX_PAYLOAD as u32,
        b"device::Switch\0",
    );
    *corrupt.last_mut().unwrap() ^= 1;
    let mut usb = FakeUsb::new(corrupt, 24);
    let error = AdbProbe::new(&mut usb).connect().unwrap_err();
    assert_eq!(error.code, "module_adb_protocol_invalid");

    let mut oversized = encode_header(ADB_CNXN, ADB_VERSION, MAX_PAYLOAD as u32, b"").unwrap();
    oversized[12..16].copy_from_slice(&((MAX_PAYLOAD as u32) + 1).to_le_bytes());
    let mut usb = FakeUsb::new(oversized.to_vec(), 24);
    let error = AdbProbe::new(&mut usb).connect().unwrap_err();
    assert_eq!(error.code, "module_adb_protocol_invalid");
}

#[test]
fn shell_status_marker_is_bound_to_random_request_token() {
    let output = b"0\n__SWITCH_ADB_STATUS_other-token_0__\n";
    let error = parse_shell_result(output, "expected-token").unwrap_err();
    assert_eq!(error.code, "module_adb_protocol_invalid");
}

#[test]
fn adb_interface_lock_is_private_exclusive_and_reusable_after_drop() {
    let root = std::env::temp_dir().join(format!(
        "switch-adb-lock-{}-{}",
        std::process::id(),
        random_token().unwrap()
    ));
    std::fs::create_dir(&root).unwrap();
    let first = acquire_interface_lock(&root).unwrap();
    let metadata = std::fs::metadata(root.join(".module-adb-interface.lock")).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    let error = acquire_interface_lock(&root).err().unwrap();
    assert_eq!(error.code, "module_adb_interface_busy");
    drop(first);
    let second = acquire_interface_lock(&root).unwrap();
    drop(second);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn adb_interface_lock_rejects_a_symlink() {
    let root = std::env::temp_dir().join(format!(
        "switch-adb-link-{}-{}",
        std::process::id(),
        random_token().unwrap()
    ));
    std::fs::create_dir(&root).unwrap();
    let target = root.join("target");
    std::fs::write(&target, b"").unwrap();
    std::os::unix::fs::symlink(&target, root.join(".module-adb-interface.lock")).unwrap();
    let error = acquire_interface_lock(&root).err().unwrap();
    assert_eq!(error.code, "module_adb_lock_unavailable");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn adb_interface_lock_preserves_legacy_readable_mode_and_rejects_writable_evidence() {
    use std::os::unix::fs::MetadataExt;
    let root = std::env::temp_dir().join(format!(
        "switch-adb-evidence-{}-{}",
        std::process::id(),
        random_token().unwrap()
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join(".module-adb-interface.lock");
    std::fs::write(&path, []).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let before = std::fs::metadata(&path).unwrap();
    let guard = acquire_interface_lock(&root).unwrap();
    guard.validate().unwrap();
    drop(guard);
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(
        (
            after.dev(),
            after.ino(),
            after.mode(),
            after.ctime(),
            after.ctime_nsec()
        ),
        (
            before.dev(),
            before.ino(),
            before.mode(),
            before.ctime(),
            before.ctime_nsec()
        )
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(
        acquire_interface_lock(&root).unwrap_err().code,
        "module_adb_lock_unavailable"
    );
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o666);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn probe_generation_is_invalidated_when_go_producer_changes() {
    let inbox = crate::message_archive::InboxState::for_go_producer("generation-a".into());
    assert!(inbox.owns_current_producer("generation-a"));
    assert!(!inbox.owns_current_producer("generation-b"));
    inbox.replace_go_producer("generation-b".into()).unwrap();
    assert!(!inbox.owns_current_producer("generation-a"));
    assert!(inbox.owns_current_producer("generation-b"));
}

#[test]
fn helper_supervisor_kills_and_reaps_a_timed_out_process() {
    let error = supervise_child(
        Command::new("/usr/bin/yes"),
        Duration::from_millis(50),
        Duration::ZERO,
    )
    .unwrap_err();
    assert_eq!(error.code, "module_adb_probe_timeout");
}

#[test]
fn helper_supervisor_maps_only_the_sanitized_authentication_marker() {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        "printf module_adb_authentication_required >&2; exit 1",
    ]);
    let error = supervise_child(command, Duration::from_secs(2), Duration::ZERO).unwrap_err();
    assert_eq!(error.code, "module_adb_authentication_required");
    assert!(!error.to_string().contains("fixture"));
}

#[test]
fn helper_supervisor_accepts_a_clean_child_exit() {
    supervise_child(
        Command::new("/usr/bin/true"),
        Duration::from_secs(2),
        Duration::ZERO,
    )
    .expect("successful helper exit should be confirmed");
}

#[test]
fn native_adb_worker_fence_survives_go_rotation_without_admitting_go() {
    let inbox = crate::message_archive::InboxState::for_go_producer("go-old".into());
    inbox
        .bind_native_module_producer("rust-module".into())
        .unwrap();
    ensure_current_producer(&inbox, "rust-module").unwrap();
    let mut readiness = VoiceRuntimeReadiness::default();
    let generation = readiness.advance_next("rust-module").unwrap();
    inbox.replace_go_producer("go-new".into()).unwrap();
    ensure_current_producer(&inbox, "rust-module").unwrap();
    readiness.validate_token("rust-module", generation).unwrap();
    assert!(ensure_current_producer(&inbox, "go-old").is_err());
    assert!(ensure_current_producer(&inbox, "go-new").is_err());
    assert!(!inbox.owns_current_producer("rust-module"));
    assert!(!inbox.compatibility_module_allowed());
}

#[tokio::test]
async fn native_audio_pipeline_completes_start_across_go_rotation_at_every_shell() {
    struct RotatingAdb {
        fake: FakeModuleVoiceADB,
        inbox: crate::message_archive::InboxState,
        rotate_at: usize,
    }
    impl ModuleVoiceADB for RotatingAdb {
        fn shell_checked_timeout(
            &mut self,
            command: &str,
            timeout: Duration,
        ) -> Result<(Vec<u8>, u32), ProbeError> {
            let reply = self.fake.shell_checked_timeout(command, timeout);
            if self.fake.commands.len() == self.rotate_at {
                self.inbox.replace_go_producer("go-new".into()).unwrap();
            }
            reply
        }
    }
    // Root check, inactive route, helper launch, and ready confirmation each
    // cross a blocking ADB boundary. Exercise the real executor and fences at
    // all four points without opening USB or launching an actual helper.
    for rotate_at in 1..=4 {
        let inbox = crate::message_archive::InboxState::for_go_producer("go-old".into());
        inbox
            .bind_native_module_producer("rust-module".into())
            .unwrap();
        let worker_inbox = inbox.clone();
        let adb_inbox = inbox.clone();
        let commands = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = commands.clone();
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release_seen = released.clone();
        let result = run_voice_route_pipeline(
            (),
            move || {
                Ok((
                    (),
                    RotatingAdb {
                        fake: FakeModuleVoiceADB::with_outputs(&[
                            ("0\n", 0),
                            ("", 1),
                            ("", 0),
                            ("", 0),
                        ]),
                        inbox: adb_inbox,
                        rotate_at,
                    },
                    true,
                ))
            },
            || async { Ok(()) },
            move |mut adb| {
                let result = module_voice_route_with_adb(
                    &mut adb,
                    VoiceRouteAction::Start,
                    StopPolicy::Always,
                    &mut || ensure_current_producer(&worker_inbox, "rust-module"),
                );
                *seen.lock().unwrap() = adb.fake.commands;
                result
            },
            move |_| async move {
                release_seen.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            },
            None,
        )
        .await
        .unwrap();
        assert!(result);
        assert!(released.load(std::sync::atomic::Ordering::Acquire));
        let commands = commands.lock().unwrap();
        assert_eq!(commands.len(), 4);
        assert!(commands[2].contains("nohup"));
        assert!(commands[3].contains("VoLTE route session active on hw:0,4"));
        assert!(
            !commands
                .iter()
                .any(|command| command.contains("kill -TERM"))
        );
        assert_eq!(inbox.current_go_producer_id().as_deref(), Some("go-new"));
    }
}

#[derive(Default)]
struct SelectedTargetTrace {
    opened: Vec<u64>,
    commands: Vec<(u64, String)>,
    pushes: Vec<u64>,
    open_results: VecDeque<Result<(), ProbeError>>,
    replies: VecDeque<Result<(String, u32), ProbeError>>,
}
struct SelectedTarget {
    id: u64,
    trace: Arc<Mutex<SelectedTargetTrace>>,
}
impl AdbTarget for SelectedTarget {
    fn open(&mut self) -> Result<Box<dyn BulkIo>, ProbeError> {
        let mut trace = self.trace.lock().unwrap();
        trace.opened.push(self.id);
        trace.open_results.pop_front().unwrap_or(Ok(()))?;
        drop(trace);
        Ok(Box::new(SelectedWireUsb {
            id: self.id,
            trace: self.trace.clone(),
            outgoing: Vec::new(),
            incoming: VecDeque::new(),
            sync: false,
        }))
    }
}
struct SelectedWireUsb {
    id: u64,
    trace: Arc<Mutex<SelectedTargetTrace>>,
    outgoing: Vec<u8>,
    incoming: VecDeque<u8>,
    sync: bool,
}
impl SelectedWireUsb {
    fn accept_frame(&mut self, tag: u32, local: u32, payload: &[u8]) -> Result<(), ProbeError> {
        if tag == ADB_CNXN {
            self.incoming.extend(frame(
                b"CNXN",
                ADB_VERSION,
                MAX_PAYLOAD as u32,
                b"device::fixture\0",
            ));
        } else if tag == ADB_OPEN {
            if payload == b"sync:\0" {
                self.sync = true;
                self.trace.lock().unwrap().pushes.push(self.id);
                self.incoming.extend(frame(b"OKAY", 42, local, b""));
            } else {
                let service =
                    String::from_utf8(payload.strip_suffix(&[0]).unwrap_or(payload).to_vec())
                        .unwrap();
                let mut trace = self.trace.lock().unwrap();
                trace.commands.push((self.id, service.clone()));
                let reply = trace.replies.pop_front().unwrap_or_else(|| {
                    Ok((
                        if service.contains("/proc/sys/kernel/random/boot_id") {
                            "12345678-1234-1234-1234-123456789abc".into()
                        } else {
                            "0".into()
                        },
                        0,
                    ))
                });
                drop(trace);
                let (output, status) = reply?;
                let token = service
                    .split("__SWITCH_ADB_STATUS_")
                    .nth(1)
                    .unwrap()
                    .split("_%u__")
                    .next()
                    .unwrap();
                let output = format!("{output}\n__SWITCH_ADB_STATUS_{token}_{status}__\n");
                self.incoming.extend(frame(b"OKAY", 42, local, b""));
                self.incoming
                    .extend(frame(b"WRTE", 42, local, output.as_bytes()));
                self.incoming.extend(frame(b"CLSE", 42, local, b""));
            }
        } else if tag == ADB_WRTE && self.sync {
            self.incoming.extend(frame(b"OKAY", 42, local, b""));
            if payload.starts_with(b"DONE") {
                self.incoming
                    .extend(frame(b"WRTE", 42, local, b"OKAY\0\0\0\0"));
            }
        } else if tag == ADB_CLSE && self.sync {
            self.incoming.extend(frame(b"CLSE", 42, local, b""));
        }
        Ok(())
    }
}
impl BulkIo for SelectedWireUsb {
    fn read_bulk(&mut self, buffer: &mut [u8], _: Duration) -> Result<usize, ProbeError> {
        assert!(!self.incoming.is_empty(), "unplanned wire read");
        let length = buffer.len().min(self.incoming.len()).min(7);
        for byte in &mut buffer[..length] {
            *byte = self.incoming.pop_front().unwrap();
        }
        Ok(length)
    }
    fn write_bulk(&mut self, payload: &[u8], _: Duration) -> Result<usize, ProbeError> {
        self.outgoing.extend_from_slice(payload);
        while self.outgoing.len() >= 24 {
            let (tag, local, _, size, _, _) = decode_header(&self.outgoing[..24])?;
            let end = 24 + size as usize;
            if self.outgoing.len() < end {
                break;
            }
            let data = self.outgoing[24..end].to_vec();
            self.outgoing.drain(..end);
            self.accept_frame(tag, local, &data)?;
        }
        Ok(payload.len())
    }
}
fn selected_plan(id: u64) -> (AdbPlan, Arc<Mutex<SelectedTargetTrace>>) {
    let trace = Arc::new(Mutex::new(SelectedTargetTrace::default()));
    (
        AdbPlan::from_target(Box::new(SelectedTarget {
            id,
            trace: trace.clone(),
        })),
        trace,
    )
}
fn selected_session(plan: AdbPlan) -> ModuleVoiceADBSession {
    ModuleVoiceADBSession::new(Instant::now() + Duration::from_secs(2), plan)
}

#[test]
fn captured_adb_selection_failure_never_invokes_the_unbound_selector() {
    // Tests only the selection policy. The unit value is not physical USB
    // evidence, and no native PhysicalTarget is fabricated by this test.
    for code in [
        "module_adb_target_unconfirmed",
        "module_adb_device_unavailable",
        TARGET_DISCONNECTED,
    ] {
        let expected = ProbeError::new(code, "captured connection cannot supply ADB");
        let result = select_captured_adb_target(
            Some(()),
            |_| Err(expected.clone()),
            || panic!("an exact target failure must not enumerate replacements"),
        );
        assert_eq!(result.err().unwrap(), expected);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn synthetic_owner_capture_is_rejected_before_native_adb_selection() {
    // The ownership fixture is accepted by actor lease tests only. It cannot
    // become a native ADB target, enumerate USB or claim physical pairing.
    let target = crate::usb_at_transport::PhysicalTarget::synthetic_for_tests();
    assert_eq!(
        AdbPlan::from_physical_target(&target).err().unwrap().code,
        "usb_at_target_unconfirmed",
    );
}

#[test]
fn absent_capture_alone_allows_the_existing_unbound_selector() {
    let expected = ProbeError::new("module_adb_device_ambiguous", "multiple unbound devices");
    let result = select_captured_adb_target(
        None::<()>,
        |_| panic!("there is no captured connection"),
        || Err(expected.clone()),
    );
    assert_eq!(result.err().unwrap(), expected);
}

#[tokio::test]
async fn rejected_captured_lease_after_preflight_submits_no_route_work() {
    // The owner validates its actual captured epoch separately. This checks
    // that the shared native pipeline honors a rejected lease without work,
    // retries or cleanup against a replacement target.
    let events = Arc::new(Mutex::new(Vec::new()));
    let prepared = events.clone();
    let acquired = events.clone();
    let worked = events.clone();
    let released = events.clone();
    let failure = run_voice_route_pipeline(
        (),
        move || {
            prepared.lock().unwrap().push("preflight");
            Ok(((), (), true))
        },
        move || async move {
            acquired.lock().unwrap().push("acquire");
            Err::<(), _>(RouteExecutionFailure::probe(
                ProbeError::new("usb_at_target_unconfirmed", "captured session changed"),
                None,
            ))
        },
        move |()| {
            worked.lock().unwrap().push("work");
            Ok(false)
        },
        move |()| async move {
            released.lock().unwrap().push("release");
            Ok(())
        },
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(failure.failure.code, "usb_at_target_unconfirmed");
    assert!(!failure.stop_confirmed);
    assert_eq!(*events.lock().unwrap(), ["preflight", "acquire"]);
}

#[test]
fn adb_interface_parser_rejects_at_and_incomplete_or_nonbulk_interfaces() {
    let endpoints = [(0x81, true), (0x02, true)];
    for (class, subclass, protocol) in [(0xff, 0, 0), (0xff, 0x42, 0), (2, 0x42, 1)] {
        assert!(adb_interface(2, 0, class, subclass, protocol, endpoints).is_none());
    }
    for endpoints in [
        vec![(0x81, true)],
        vec![(0x02, true)],
        vec![(0x81, false), (0x02, true)],
        vec![(0x81, true), (0x02, false)],
        vec![(0x80, true), (0x00, true)],
    ] {
        assert!(adb_interface(5, 0, 0xff, 0x42, 1, endpoints).is_none());
    }
}

#[test]
fn adb_interface_parser_preserves_the_actual_interface_alternate_and_bulk_endpoints() {
    assert_eq!(
        adb_interface(
            6,
            2,
            0xff,
            0x42,
            1,
            [(0x85, true), (0x04, true), (0x87, false)],
        ),
        Some(AdbInterface {
            interface: 6,
            alternate: 2,
            endpoint_in: 0x85,
            endpoint_out: 0x04,
        }),
    );
}

#[test]
fn selected_adb_requires_one_candidate_before_any_target_is_opened() {
    let trace = Arc::new(Mutex::new(SelectedTargetTrace::default()));
    let candidates = vec![
        SelectedTarget {
            id: 1,
            trace: trace.clone(),
        },
        SelectedTarget {
            id: 2,
            trace: trace.clone(),
        },
    ];
    assert_eq!(
        unique_adb_target(candidates).err().unwrap().code,
        "module_adb_device_ambiguous"
    );
    assert!(trace.lock().unwrap().opened.is_empty());
    assert_eq!(
        unique_adb_target::<SelectedTarget>(Vec::new())
            .err()
            .unwrap()
            .code,
        "module_adb_device_unavailable"
    );
    let target = unique_adb_target(vec![SelectedTarget {
        id: 3,
        trace: trace.clone(),
    }])
    .unwrap();
    let mut session = selected_session(AdbPlan::from_target(Box::new(target)));
    module_voice_route_root(&mut session).unwrap();
    assert_eq!(trace.lock().unwrap().opened, [3]);
}

#[test]
fn selected_adb_open_failure_never_falls_back_and_same_target_cleanup_remains_possible() {
    let (plan, trace) = selected_plan(11);
    let original = ProbeError::new(
        "module_adb_device_unavailable",
        "original target claim failed",
    );
    trace
        .lock()
        .unwrap()
        .open_results
        .push_back(Err(original.clone()));
    let mut session = selected_session(plan);
    assert_eq!(module_voice_route_root(&mut session).unwrap_err(), original);
    // A normal failure does not abandon the original target's cleanup right.
    stop_module_voice_route(
        &mut session,
        1,
        Duration::ZERO,
        1,
        Duration::ZERO,
        &mut || Ok(()),
    )
    .unwrap();
    let trace = trace.lock().unwrap();
    assert_eq!(trace.opened, [11, 11, 11, 11]);
    assert_eq!(trace.commands.len(), 3);
    assert!(trace.commands.iter().all(|(id, _)| *id == 11));
    assert!(trace.commands[0].1.contains("helper_stopped=1"));
}

#[test]
fn selected_adb_session_root_status_and_stop_reopen_only_the_same_opaque_target() {
    let (plan, trace) = selected_plan(23);
    let mut session = selected_session(plan);
    module_voice_route_root(&mut session).unwrap();
    assert!(module_voice_route_status(&mut session).unwrap());
    assert!(
        !module_voice_route_with_adb(
            &mut session,
            VoiceRouteAction::Stop,
            StopPolicy::Always,
            &mut || Ok(())
        )
        .unwrap()
    );
    let trace = trace.lock().unwrap();
    assert_eq!(trace.opened, [23; 6]);
    assert_eq!(trace.commands.len(), 6);
    assert!(trace.commands.iter().all(|(id, _)| *id == 23));
    assert!(trace.commands[3].1.contains("helper_stopped=1"));
    assert!(
        trace.commands[5]
            .1
            .contains("echo 0 > /sys/class/android_usb/f_audio/audio_enable")
    );
}

#[test]
fn selected_adb_unknown_stop_reply_can_reconcile_cleanup_on_original_target() {
    let (plan, trace) = selected_plan(31);
    trace.lock().unwrap().replies.extend([
        Err(ProbeError::new(
            "module_adb_usb_timeout",
            "lost original stop reply",
        )),
        Ok((String::new(), 0)), // helper stopped
        Ok((String::new(), 0)), // real T/T/B + UAC disable cleanup
        Ok((String::new(), 1)), // not active
        Ok((String::new(), 0)), // stopped
    ]);
    let mut session = selected_session(plan);
    stop_module_voice_route(
        &mut session,
        1,
        Duration::ZERO,
        1,
        Duration::ZERO,
        &mut || Ok(()),
    )
    .unwrap();
    let trace = trace.lock().unwrap();
    assert_eq!(trace.opened, [31; 5]);
    assert_eq!(trace.commands.len(), 5);
    assert!(
        trace.commands[2]
            .1
            .contains("echo 0 > /sys/class/android_usb/f_audio/audio_enable")
    );
    assert!(trace.commands.iter().all(|(id, _)| *id == 31));
}

#[test]
fn selected_adb_disconnected_open_retains_original_error_without_reopening_replacement() {
    let (plan, trace) = selected_plan(41);
    let disconnected = ProbeError::new(TARGET_DISCONNECTED, "original connection vanished");
    trace
        .lock()
        .unwrap()
        .open_results
        .extend([Err(disconnected.clone()), Ok(())]);
    let mut first = selected_session(plan.clone());
    assert_eq!(
        module_voice_route_root(&mut first).unwrap_err(),
        disconnected
    );
    let mut cleanup = selected_session(plan);
    assert_eq!(
        cleanup
            .shell_checked_timeout(module_voice_route_stop_command(), Duration::from_secs(1))
            .unwrap_err(),
        disconnected
    );
    let trace = trace.lock().unwrap();
    assert_eq!(trace.opened, [41]);
    assert_eq!(
        trace.open_results.len(),
        1,
        "replacement open was attempted"
    );
    assert!(trace.commands.is_empty());
}

#[test]
fn selected_adb_disconnect_during_command_fences_later_shell_and_keeps_stop_unknown() {
    let (plan, trace) = selected_plan(43);
    let disconnected = ProbeError::new(TARGET_DISCONNECTED, "original command lost its connection");
    trace
        .lock()
        .unwrap()
        .replies
        .extend([Ok(("0".into(), 0)), Err(disconnected.clone())]);
    let mut session = selected_session(plan);
    let failure = module_voice_route_with_adb(
        &mut session,
        VoiceRouteAction::Stop,
        StopPolicy::Always,
        &mut || Ok(()),
    )
    .unwrap_err();
    assert!(!failure.stop_confirmed);
    assert_eq!(failure.failure.code, TARGET_DISCONNECTED);
    assert!(
        failure
            .failure
            .message
            .contains("original command lost its connection")
    );
    let trace = trace.lock().unwrap();
    assert_eq!(trace.opened, [43, 43]);
    assert_eq!(trace.commands.len(), 2);
    assert!(trace.commands[1].1.contains("helper_stopped=1"));
}

#[test]
fn selected_adb_prepare_push_selftest_cleanup_and_boot_use_one_plan() {
    struct Preparation {
        plan: AdbPlan,
        events: Vec<&'static str>,
    }
    impl RuntimePreparationIO for Preparation {
        fn deploy(
            &mut self,
            current: &mut dyn FnMut() -> Result<(), ProbeError>,
        ) -> Result<String, ProbeError> {
            current()?;
            self.events.push("push");
            self.plan.with_usb(|usb| {
                let mut adb = AdbProbe::new(usb);
                adb.connect()?;
                adb.push(b"synthetic", "/tmp/proletarius-call/fixture", 0o100700)
            })?;
            Ok("12345678-1234-1234-1234-123456789abc".into())
        }
        fn route(
            &mut self,
            action: VoiceRouteAction,
            current: &mut dyn FnMut() -> Result<(), ProbeError>,
        ) -> Result<bool, ProbeError> {
            self.events.push(match action {
                VoiceRouteAction::Start => "start",
                VoiceRouteAction::Stop => "stop",
                VoiceRouteAction::Status => "status",
            });
            let mut session = selected_session(self.plan.clone());
            module_voice_route_with_adb(&mut session, action, StopPolicy::Always, &mut || current())
                .map_err(|error| ProbeError::new(error.failure.code, error.failure.message))
        }
        fn rollback(
            &mut self,
            current: &mut dyn FnMut() -> Result<(), ProbeError>,
        ) -> Result<(), ProbeError> {
            current()?;
            self.events.push("rollback");
            let mut session = selected_session(self.plan.clone());
            assert!(module_voice_route_stopped(&mut session)?);
            cleanup_module_voice_audio(&mut session, 1, Duration::ZERO, &mut || current())
        }
        fn boot_id(
            &mut self,
            current: &mut dyn FnMut() -> Result<(), ProbeError>,
        ) -> Result<String, ProbeError> {
            current()?;
            self.events.push("boot");
            read_module_boot_id(&mut selected_session(self.plan.clone()))
        }
    }
    let (plan, trace) = selected_plan(51);
    let mut preparation = Preparation {
        plan,
        events: Vec::new(),
    };
    assert_eq!(
        prepare_runtime_pipeline(&mut preparation, &mut || Ok(())).unwrap(),
        "12345678-1234-1234-1234-123456789abc"
    );
    preparation.rollback(&mut || Ok(())).unwrap();
    assert_eq!(
        preparation.events,
        ["push", "start", "stop", "boot", "rollback"]
    );
    let trace = trace.lock().unwrap();
    assert_eq!(trace.pushes, [51]);
    assert_eq!(trace.opened, [51; 10]);
    assert_eq!(trace.commands.len(), 9);
    assert!(trace.commands.iter().all(|(id, _)| *id == 51));
    assert_eq!(
        trace
            .commands
            .iter()
            .filter(|(_, command)| command
                .contains("echo 0 > /sys/class/android_usb/f_audio/audio_enable"))
            .count(),
        2
    );
}

#[test]
fn selected_adb_start_disconnect_does_not_launch_or_rollback_on_another_device() {
    let (plan, trace) = selected_plan(61);
    let disconnected = ProbeError::new(TARGET_DISCONNECTED, "original ready query lost device");
    trace.lock().unwrap().replies.extend([
        Ok(("0".into(), 0)),       // root
        Err(disconnected.clone()), // readiness on the original target
    ]);
    let mut session = selected_session(plan);
    let failure = module_voice_route_with_adb(
        &mut session,
        VoiceRouteAction::Start,
        StopPolicy::Always,
        &mut || Ok(()),
    )
    .unwrap_err();
    assert!(!failure.stop_confirmed);
    assert_eq!(failure.failure.code, TARGET_DISCONNECTED);
    assert_eq!(failure.failure.message, disconnected.message);
    let trace = trace.lock().unwrap();
    assert_eq!(trace.opened, [61, 61]);
    assert_eq!(trace.commands.len(), 2);
    assert!(
        trace
            .commands
            .iter()
            .all(|(id, command)| *id == 61 && !command.contains("nohup"))
    );
}

fn cleanup_fixture(producer: &str, generation: u64, plan: AdbPlan) -> VoiceCleanupObligation {
    let gate = crate::module_recovery::Gate::default();
    VoiceCleanupObligation {
        producer: producer.into(),
        generation,
        target: VoiceBoundTarget {
            plan,
            capture: None,
            managed: None,
        },
        authority: Arc::new(gate.admit_operation(producer).unwrap()),
    }
}

#[path = "restore_settlement_tests.rs"]
mod restore_settlement_tests;

#[test]
fn readiness_invalidation_does_not_erase_original_route_cleanup_target() {
    let (plan, trace) = selected_plan(5791);
    let mut ownership = VoiceRouteOwnership::default();
    let obligation = cleanup_fixture("producer-a", 1, plan.clone());
    ownership.begin(obligation.clone()).unwrap();
    let mut readiness = VoiceRuntimeReadiness::default();
    readiness.advance_next("producer-a").unwrap();
    readiness
        .mark_prepared("producer-a", 1, "boot-a".into())
        .unwrap();
    readiness.prepared_target = Some(obligation.target.clone());
    readiness.invalidate_prepared("producer-a", 1).unwrap();
    assert!(readiness.prepared_target.is_none());
    readiness.advance_next("producer-a").unwrap();
    let retained = ownership.retained("producer-a").unwrap().unwrap();
    let mut session = selected_session(retained.target.plan);
    session
        .shell_checked_timeout("id -u", Duration::from_secs(1))
        .unwrap();
    assert_eq!(trace.lock().unwrap().opened, [5791]);
    assert_eq!(
        ownership
            .retained("producer-a")
            .unwrap()
            .unwrap()
            .generation,
        1
    );
}

#[test]
fn outstanding_unknown_route_rejects_replacement_generation_or_producer() {
    let (plan, trace) = selected_plan(5792);
    let original = cleanup_fixture("producer-a", 7, plan);
    let mut ownership = VoiceRouteOwnership::default();
    ownership.begin(original.clone()).unwrap();
    // A retry of the exact accepted plan is idempotent. A new AT target,
    // readiness generation or producer cannot replace the cleanup owner.
    ownership.begin(original.clone()).unwrap();
    for (producer, generation) in [("producer-a", 8), ("producer-b", 1), ("producer-a", 7)] {
        let (replacement, replacement_trace) = selected_plan(5793);
        let error = ownership
            .begin(cleanup_fixture(producer, generation, replacement))
            .unwrap_err();
        assert_eq!(error.code, "module_voice_cleanup_pending");
        assert!(replacement_trace.lock().unwrap().opened.is_empty());
    }
    assert_eq!(ownership.obligations.len(), 1);
    assert!(trace.lock().unwrap().opened.is_empty());
}

#[test]
fn disappeared_original_route_plan_never_opens_a_later_candidate() {
    let (plan, trace) = selected_plan(5794);
    trace
        .lock()
        .unwrap()
        .open_results
        .push_back(Err(ProbeError::new(
            TARGET_DISCONNECTED,
            "old connection gone",
        )));
    let (later_candidate, later_trace) = selected_plan(5795);
    let mut ownership = VoiceRouteOwnership::default();
    ownership
        .begin(cleanup_fixture("producer-a", 2, plan))
        .unwrap();
    for _ in 0..2 {
        let retained = ownership.retained("producer-a").unwrap().unwrap();
        let mut session = selected_session(retained.target.plan);
        assert_eq!(
            session
                .shell_checked_timeout("id -u", Duration::from_secs(1))
                .unwrap_err()
                .code,
            TARGET_DISCONNECTED
        );
    }
    assert_eq!(trace.lock().unwrap().opened, [5794]);
    assert!(
        ownership
            .begin(cleanup_fixture("producer-a", 3, later_candidate))
            .is_err()
    );
    assert!(later_trace.lock().unwrap().opened.is_empty());
    assert_eq!(ownership.obligations.len(), 1);
}

#[test]
fn cleanup_retirement_requires_confirmed_stop_release_and_exact_obligation() {
    let (plan, _) = selected_plan(5796);
    let original = cleanup_fixture("producer-a", 4, plan);
    let mut ownership = VoiceRouteOwnership::default();
    ownership.begin(original.clone()).unwrap();
    ownership.retire(&original, false); // unknown stop or USB release
    assert!(ownership.retained("producer-a").unwrap().is_some());
    let (other, _) = selected_plan(5797);
    ownership.retire(&cleanup_fixture("producer-a", 4, other), true);
    assert!(ownership.retained("producer-a").unwrap().is_some());
    let mut different_generation = original.clone();
    different_generation.generation = 5;
    ownership.retire(&different_generation, true);
    assert!(ownership.retained("producer-a").unwrap().is_some());
    ownership.retire(&original, true);
    assert!(ownership.retained("producer-a").unwrap().is_none());
}

#[test]
fn prepared_target_is_cleared_on_boot_change_without_retiring_live_route() {
    let (plan, _) = selected_plan(5798);
    let obligation = cleanup_fixture("producer-a", 1, plan);
    let mut readiness = VoiceRuntimeReadiness::default();
    readiness.advance_next("producer-a").unwrap();
    readiness
        .mark_prepared("producer-a", 1, "boot-a".into())
        .unwrap();
    readiness.prepared_target = Some(obligation.target.clone());
    let mut ownership = VoiceRouteOwnership::default();
    ownership.begin(obligation).unwrap();
    assert!(!readiness.is_prepared("producer-a", 1, "boot-b"));
    assert!(readiness.prepared_target.is_none());
    assert!(ownership.retained("producer-a").unwrap().is_some());
}

#[tokio::test]
async fn cancelled_unknown_start_keeps_cross_request_obligation_after_worker_cleanup() {
    let (plan, _) = selected_plan(5799);
    let obligation = cleanup_fixture("producer-a", 9, plan);
    let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
    ownership.lock().unwrap().begin(obligation.clone()).unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (finish_work, wait_work) = std::sync::mpsc::channel();
    let (cleaned, wait_cleaned) = tokio::sync::oneshot::channel();
    let worker = spawn_guarded_voice_task(None, async move {
        run_voice_route_worker(
            (),
            move || {
                started.send(()).unwrap();
                wait_work.recv_timeout(Duration::from_secs(3)).unwrap();
                Err(RouteExecutionFailure::probe(
                    ProbeError::new(
                        "module_voice_route_timeout",
                        "Start acknowledgement unknown",
                    ),
                    None,
                ))
            },
            async move {
                cleaned.send(()).unwrap();
                Ok(())
            },
            None,
        )
        .await
    });
    let observer = tokio::spawn(async move { worker.await });
    ready.await.unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    finish_work.send(()).unwrap();
    wait_cleaned.await.unwrap();
    // Lease release alone never says the Start/route effects were stopped.
    let retained = ownership
        .lock()
        .unwrap()
        .retained("producer-a")
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(
        &retained.target.plan.0,
        &obligation.target.plan.0
    ));
    assert_eq!(retained.generation, 9);
}

#[tokio::test]
async fn final_shutdown_revoked_empty_registry_never_dispatches_new_adb_work() {
    let gate = crate::module_recovery::Gate::default();
    gate.revoke_for_tests();
    assert!(gate.admit_operation("producer-a").is_err());
    let ownership = VoiceRouteOwnership::default();
    let semaphore = Semaphore::new(1);
    let receipt = finish_final_voice_cleanup(
        &semaphore,
        || {
            ownership
                .final_settlement("producer-a")
                .map_err(|error| error.to_string())
        },
        |_| async { panic!("empty registry must not capture, enumerate, admit or submit Stop") },
    )
    .await
    .unwrap();
    assert!(receipt.is_empty());
    assert_eq!(semaphore.available_permits(), 1);
}

#[tokio::test]
async fn final_shutdown_prepared_only_is_no_owned_cleanup_after_live_revoke() {
    let (plan, trace) = selected_plan(5800);
    let mut readiness = VoiceRuntimeReadiness::default();
    readiness.advance_next("producer-a").unwrap();
    readiness
        .mark_prepared("producer-a", 1, "boot-a".into())
        .unwrap();
    readiness.prepared_target = Some(VoiceBoundTarget {
        plan,
        capture: None,
        managed: None,
    });
    let gate = crate::module_recovery::Gate::default();
    gate.revoke_for_tests();
    let ownership = VoiceRouteOwnership::default();
    let receipt = finish_final_voice_cleanup(
        &Semaphore::new(1),
        || {
            ownership
                .final_settlement("producer-a")
                .map_err(|error| error.to_string())
        },
        |_| async { panic!("prepared self-test was already stopped and released") },
    )
    .await
    .unwrap();
    assert!(gate.is_admission_revoked());
    assert!(receipt.is_empty());
    assert!(readiness.prepared_target.is_some());
    assert!(trace.lock().unwrap().opened.is_empty());
}

#[tokio::test]
async fn final_shutdown_other_producer_obligation_remains_unconfirmed() {
    let (plan, trace) = selected_plan(5801);
    let mut ownership = VoiceRouteOwnership::default();
    ownership
        .begin(cleanup_fixture("old-producer", 1, plan))
        .unwrap();
    let error = finish_final_voice_cleanup(
        &Semaphore::new(1),
        || {
            ownership
                .final_settlement("new-producer")
                .map_err(|error| error.to_string())
        },
        |_| async { panic!("another producer's obligation cannot be replaced or settled") },
    )
    .await
    .unwrap_err();
    assert!(error.contains("其他后端"));
    assert!(ownership.retained("old-producer").unwrap().is_some());
    assert!(trace.lock().unwrap().opened.is_empty());
}

#[tokio::test]
async fn final_shutdown_drains_accepted_worker_before_inspecting_registry() {
    let semaphore = Arc::new(Semaphore::new(1));
    let accepted = semaphore.clone().acquire_owned().await.unwrap();
    let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
    let inspecting = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let final_admission = semaphore.clone();
    let final_ownership = ownership.clone();
    let final_inspecting = inspecting.clone();
    let settlement_admission = semaphore.clone();
    let finalizer = tokio::spawn(async move {
        finish_final_voice_cleanup(
            &final_admission,
            || {
                final_inspecting.store(true, std::sync::atomic::Ordering::SeqCst);
                final_ownership
                    .lock()
                    .unwrap()
                    .final_settlement("producer-a")
                    .map_err(|error| error.to_string())
            },
            |permit| async move {
                let _permit = permit;
                assert_eq!(
                    settlement_admission.available_permits(),
                    0,
                    "drained admission must remain held through exact settlement"
                );
                Ok(vec!["retained exact settlement reached".into()])
            },
        )
        .await
    });
    tokio::task::yield_now().await;
    assert!(!inspecting.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!finalizer.is_finished());
    // An accepted worker publishes its obligation while still holding ADB
    // admission. An earlier empty-registry check would miss this route.
    let (plan, trace) = selected_plan(5802);
    ownership
        .lock()
        .unwrap()
        .begin(cleanup_fixture("producer-a", 2, plan))
        .unwrap();
    drop(accepted);
    let receipt = finalizer.await.unwrap().unwrap();
    assert_eq!(receipt, ["retained exact settlement reached"]);
    assert!(inspecting.load(std::sync::atomic::Ordering::SeqCst));
    assert!(trace.lock().unwrap().opened.is_empty());
    assert_eq!(semaphore.available_permits(), 1);
}

#[tokio::test]
async fn final_shutdown_waits_through_confirmed_stop_until_obligation_retirement() {
    let semaphore = Arc::new(Semaphore::new(1));
    let accepted = semaphore.clone().acquire_owned().await.unwrap();
    let (plan, trace) = selected_plan(5803);
    let obligation = cleanup_fixture("producer-a", 3, plan);
    let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
    ownership.lock().unwrap().begin(obligation.clone()).unwrap();
    let worker_ownership = ownership.clone();
    let (physically_settled, settled) = tokio::sync::oneshot::channel();
    let (retire, allow_retirement) = tokio::sync::oneshot::channel();
    let worker = spawn_guarded_voice_task(None, async move {
        let _permit = accepted;
        let result =
            run_voice_route_worker((), || Ok(false), async { Ok(()) }, Some(StopPhase::Stop)).await;
        assert!(matches!(result, Ok(false)));
        physically_settled.send(()).unwrap();
        allow_retirement.await.unwrap();
        worker_ownership.lock().unwrap().retire(&obligation, true);
    });
    settled.await.unwrap();
    let final_admission = semaphore.clone();
    let final_ownership = ownership.clone();
    let finalizer = tokio::spawn(async move {
        finish_final_voice_cleanup(
            &final_admission,
            || {
                final_ownership
                    .lock()
                    .unwrap()
                    .final_settlement("producer-a")
                    .map_err(|error| error.to_string())
            },
            |_| async { panic!("completed worker must retire before final no-op decision") },
        )
        .await
    });
    tokio::task::yield_now().await;
    assert!(!finalizer.is_finished());
    retire.send(()).unwrap();
    worker.await.unwrap();
    assert!(finalizer.await.unwrap().unwrap().is_empty());
    assert!(ownership.lock().unwrap().obligations.is_empty());
    assert!(trace.lock().unwrap().opened.is_empty());
}

#[tokio::test]
async fn stopping_unowned_voice_status_is_refused_before_capture_or_unbound_selection() {
    for terminal in [false, true] {
        let f = crate::module_radio::tests::tests::Fixture::new();
        f.service.begin_shutdown();
        if terminal {
            assert!(
                f.service
                    .finish_shutdown_receipt()
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        // A mismatched current-producer DTO is a second, safe stop before any
        // hardware in the vulnerable baseline. Fixed admission must reject even
        // earlier, before that DTO fence, USB capture, or unbound selection.
        let inbox = crate::message_archive::InboxState::for_go_producer("not-producer-1".into());
        let error = execute_voice_route_action(
            &f.root,
            inbox,
            crate::usb_at::Owner::unavailable_for_tests("producer-1".into()),
            f.service.clone(),
            "producer-1".into(),
            VoiceRouteAction::Status,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "backend_stopping");
        assert!(!f.root.join(".module-adb-interface.lock").exists());
    }
}
