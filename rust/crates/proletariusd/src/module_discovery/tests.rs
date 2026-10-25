use super::*;
use std::os::unix::fs::PermissionsExt;

const DEVICE: &str = "  \"idVendor\" = 11427\n  \"idProduct\" = 4660\n  \"USB Product Name\" = \"DJI modem\"\n  \"locationID\" = 4294967295\n  \"USBSpeed\" = 4\n  \"bInterfaceNumber\" = 2\n  \"bInterfaceClass\" = 255\n  \"bInterfaceSubClass\" = 3\n  \"bInterfaceProtocol\" = 7\n  \"bNumEndpoints\" = 2\n";

struct Fixture {
    root: PathBuf,
    observer: Observer,
}
impl Fixture {
    fn new(body: &str, timeout: Duration, max_bytes: usize) -> Self {
        let root =
            std::env::temp_dir().join(format!("discovery-{}", crate::snapshot::new_instance_id()));
        std::fs::create_dir_all(&root).unwrap();
        let program = root.join("ioreg");
        std::fs::write(&program, format!("#!/bin/sh\ncd \"$(dirname \"$0\")\" || exit 1\nprintf '%s\\n' \"$$\" >> invocations\n{body}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.join("output"), DEVICE).unwrap();
        Self {
            root,
            observer: Observer(Arc::new(Inner {
                state: Mutex::new(State::default()),
                program,
                args: vec![],
                timeout,
                ttl: Duration::from_secs(60),
                max_bytes,
                test_result: None,
            })),
        }
    }
    async fn entered(&self, count: usize) -> u32 {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(self.root.join("invocations")) {
                    let lines: Vec<_> = text.lines().collect();
                    if lines.len() >= count {
                        return lines[count - 1].parse().unwrap();
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("actual subprocess never entered")
    }
    fn count(&self) -> usize {
        std::fs::read_to_string(self.root.join("invocations"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
    fn release(&self) {
        std::fs::write(self.root.join("release"), "").unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn assert_reaped(pid: u32) {
    let result = std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        !result.success(),
        "subprocess {pid} remains alive after observation completion"
    );
}

#[tokio::test]
async fn cancelled_waiter_retains_actual_worker_and_concurrent_waiters_share_it() {
    let fixture = Fixture::new(
        "while [ ! -f release ]; do :; done\nexec /bin/cat output",
        Duration::from_secs(5),
        4096,
    );
    let observer = fixture.observer.clone();
    let first = tokio::spawn(async move { observer.observe(7).await });
    let pid = fixture.entered(1).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let observer = fixture.observer.clone();
    let second = tokio::spawn(async move { observer.observe(7).await });
    tokio::task::yield_now().await;
    assert!(!second.is_finished());
    assert_eq!(fixture.count(), 1);
    fixture.release();
    assert_eq!(second.await.unwrap().unwrap().unwrap().product, "DJI modem");
    assert_reaped(pid);
    assert!(fixture.observer.observe(7).await.unwrap().is_some());
    assert_eq!(
        fixture.count(),
        1,
        "same physical epoch must reuse completed cache"
    );
}

#[tokio::test]
async fn replacement_epoch_never_uses_old_cached_identity_or_overlaps_old_process() {
    let fixture = Fixture::new(
        "while [ ! -f release ]; do :; done\nexec /bin/cat output",
        Duration::from_secs(5),
        4096,
    );
    let observer = fixture.observer.clone();
    let first = tokio::spawn(async move { observer.observe(1).await });
    let old_pid = fixture.entered(1).await;
    let observer = fixture.observer.clone();
    let next = tokio::spawn(async move { observer.observe(2).await });
    tokio::task::yield_now().await;
    assert!(!next.is_finished());
    assert_eq!(fixture.count(), 1);
    fixture.release();
    first.await.unwrap().unwrap();
    next.await.unwrap().unwrap();
    let next_pid = fixture.entered(2).await;
    assert_reaped(old_pid);
    assert_reaped(next_pid);
    assert_eq!(fixture.count(), 2);
    std::fs::write(
        fixture.root.join("output"),
        DEVICE.replace("DJI modem", "Replacement modem"),
    )
    .unwrap();
    let replacement = fixture.observer.observe(3).await.unwrap().unwrap();
    assert_eq!(replacement.product, "Replacement modem");
    assert_eq!(fixture.count(), 3);
}

#[tokio::test]
async fn timeout_and_output_limit_wait_for_real_child_exit() {
    for (body, timeout, limit, expected) in [
        (
            "while :; do :; done",
            Duration::from_millis(100),
            4096,
            "ioreg_timeout",
        ),
        (
            "printf '01234567890123456789'\nwhile :; do :; done",
            Duration::from_secs(5),
            8,
            "ioreg_output_limit",
        ),
    ] {
        let fixture = Fixture::new(body, timeout, limit);
        let observer = fixture.observer.clone();
        let job = tokio::spawn(async move { observer.observe(1).await });
        let pid = fixture.entered(1).await;
        assert_eq!(job.await.unwrap(), Err(expected));
        assert_reaped(pid);
    }
}

#[tokio::test]
async fn absent_and_failed_discovery_remain_distinct_and_do_not_publish_raw_output() {
    for (body, expected_source, error_code) in [
        ("printf 'no devices'", ObservationSource::Absent, "未检测到"),
        (
            "printf 'secret-imsi'; exit 9",
            ObservationSource::Unavailable,
            "ioreg_failed",
        ),
        (
            "printf '\\377'",
            ObservationSource::Unavailable,
            "ioreg_invalid_utf8",
        ),
    ] {
        let fixture = Fixture::new(body, Duration::from_secs(5), 4096);
        let usb = crate::usb_at::Owner::unavailable_for_tests("observation-test".into());
        let result = fixture
            .observer
            .capture_usb_fallback(&usb, Some("/dev/selected"), 1)
            .await;
        assert_eq!(result.source, expected_source);
        assert_eq!(result.port, "/dev/selected");
        assert!(result.device.is_none());
        assert!(result.discovery_error.contains(error_code));
        assert!(!result.discovery_error.contains("secret-imsi"));
        assert_reaped(fixture.entered(1).await);
        usb.shutdown().await.unwrap();
    }
}

struct Port;
impl crate::usb_at_transport::Transport for Port {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn description(&self) -> String {
        "exact native USB description".into()
    }
    fn command(
        &mut self,
        _: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, crate::usb_at_transport::TransportError> {
        panic!("observation must never send AT")
    }
}
#[tokio::test]
async fn open_usb_uses_actual_owner_description_without_running_ioreg_or_at() {
    let fixture = Fixture::new("exit 99", Duration::from_secs(5), 4096);
    let (usb, _) =
        crate::usb_at::Owner::native_module_fixture(Arc::new(|| Ok(Box::new(Port)))).await;
    let result = fixture
        .observer
        .capture_usb_fallback(&usb, Some("ignored serial"), 4)
        .await;
    assert_eq!(result.source, ObservationSource::NativeUsb);
    assert_eq!(result.port, "exact native USB description");
    assert!(result.discovery_error.is_empty());
    let device = result.device.unwrap();
    assert_eq!(device.product, result.port);
    assert_eq!(device.mode, "USB AT");
    assert!(device.vendor.is_empty() && device.interfaces.is_none());
    assert_eq!(fixture.count(), 0);
    usb.shutdown().await.unwrap();
}

#[test]
fn legacy_dto_preserves_large_location_and_nil_interfaces() {
    let device = crate::network_diagnostic::parse_health_usb_device(DEVICE).unwrap();
    assert_eq!(device.vendor_id, "2ca3");
    assert_eq!(device.product_id, "1234");
    assert_eq!(device.vendor, "DJI");
    assert_eq!(device.location_id, "0xffffffff");
    assert_eq!(device.speed, "super-speed");
    assert_eq!(device.mode, "vendor-specific USB interface");
    let interface = &device.interfaces.unwrap()[0];
    assert_eq!(
        (
            interface.number,
            interface.class_id,
            interface.subclass,
            interface.protocol_id,
            interface.endpoints
        ),
        (2, 255, 3, 7, 2)
    );
    let minimal = crate::network_diagnostic::parse_health_usb_device(
        "\"idVendor\" = 11427\n\"idProduct\" = 1",
    )
    .unwrap();
    assert_eq!(minimal.product, "DJI 4G Module");
    assert!(minimal.interfaces.is_none());
    assert!(
        crate::network_diagnostic::parse_health_usb_device("\"idVendor\" = 1\n\"idProduct\" = 1")
            .is_none()
    );
}

#[test]
fn numeric_fields_match_legacy_unsigned_decimal_prefix_and_signed_64_bit_overflow() {
    for (raw, expected) in [
        ("2147483648", "0x80000000"),
        ("9223372036854775807", "0x7fffffffffffffff"),
        ("9223372036854775808", ""),
        ("-1", ""),
        ("+1", ""),
        ("123 trailing", "0x7b"),
    ] {
        let output = format!("\"idVendor\" = 11427\n\"idProduct\" = 1\n\"locationID\" = {raw}");
        assert_eq!(
            crate::network_diagnostic::parse_health_usb_device(&output)
                .unwrap()
                .location_id,
            expected,
            "{raw}"
        );
    }
}

#[test]
fn legacy_properties_match_first_valid_regexp_capture_and_sort_interfaces() {
    let output = concat!(
        "\"idVendor\" = -1\n\"idVendor\" = 11427\n\"idProduct\" = 12 suffix\n",
        "\"USB Product Name\" = unquoted\n\"USB Product Name\" = \"first product\" ignored\n",
        "\"bInterfaceNumber\" = 7\n\"bInterfaceClass\" = 255\n\n",
        "\"idVendor\" = 11427\n\"idProduct\" = 12\n\"bInterfaceNumber\" = 1\n\"bInterfaceClass\" = 2",
    );
    let result = crate::network_diagnostic::parse_health_usb_device(output).unwrap();
    assert_eq!(result.product, "first product");
    assert_eq!(result.product_id, "000c");
    assert_eq!(result.mode, "vendor-specific USB mode");
    assert_eq!(
        result
            .interfaces
            .unwrap()
            .iter()
            .map(|i| i.number)
            .collect::<Vec<_>>(),
        [1, 7]
    );
    assert!(
        crate::network_diagnostic::parse_health_usb_device("idVendor = 11427\n\"idProduct\" = 1")
            .is_none()
    );
}
