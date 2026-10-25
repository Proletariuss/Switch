use super::{
    INTERNAL_PREFIX, ProfileNote, Store, handle_internal, handle_public, is_missing_phone,
    normalize_sim_identifier,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

fn test_root() -> PathBuf {
    let path = std::env::var_os("SWITCH_BUILD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../build"))
        .join("runtime/profile-notes-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            super::NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn full_request(method: Method, path: &str, body: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method(method)
        .uri(path)
        .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
        .unwrap()
}

async fn response_json(response: hyper::Response<Full<Bytes>>) -> (StatusCode, Value) {
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[test]
fn legacy_profile_notes_file_round_trips_in_object_format_and_remembers_phone() {
    let root = test_root();
    let path = root.join("profile-notes.json");
    fs::write(
        &path,
        br#"{"8901234567890123456":{"label":"Travel","phone":"","tags":"work","future":"preserve across versions"}}"#,
    )
    .unwrap();
    let store = Store::new(&root);
    assert_eq!(store.phone_for_iccid("8901234567890123456F").unwrap(), "");
    assert!(
        store
            .remember_phone("8901234567890123456", "+447700900123")
            .unwrap()
    );
    assert_eq!(
        store.phone_for_iccid("\"8901234567890123456F\"").unwrap(),
        "+447700900123"
    );
    let notes: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(notes["8901234567890123456"]["label"], "Travel");
    assert_eq!(notes["8901234567890123456"]["tags"], "work");
    assert_eq!(notes["8901234567890123456"]["phone"], "+447700900123");
    assert_eq!(
        notes["8901234567890123456"]["future"],
        "preserve across versions"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn profile_note_update_trims_unicode_and_empty_values_delete() {
    let root = test_root();
    let store = Store::new(&root);
    let note = ProfileNote {
        label: "  通勤卡  ".into(),
        phone: " +441234567890 ".into(),
        tags: "  UK  ".into(),
    };
    assert_eq!(
        store
            .save_note(
                "8901".into(),
                ProfileNote {
                    label: note.label.trim().into(),
                    phone: note.phone.trim().into(),
                    tags: note.tags.trim().into(),
                }
            )
            .unwrap(),
        ProfileNote {
            label: "通勤卡".into(),
            phone: "+441234567890".into(),
            tags: "UK".into(),
        }
    );
    assert_eq!(store.load().unwrap().len(), 1);
    assert_eq!(
        store
            .save_note("8901".into(), ProfileNote::default())
            .unwrap(),
        ProfileNote::default()
    );
    assert!(store.load().unwrap().is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn note_store_is_private_atomic_and_preserves_corrupt_source() {
    let root = test_root();
    let store = Store::new(&root);
    store
        .save_note(
            "8901".into(),
            ProfileNote {
                label: "home".into(),
                ..ProfileNote::default()
            },
        )
        .unwrap();
    assert_eq!(
        fs::metadata(root.join("profile-notes.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(root.join("profile-notes.json.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let corrupt = b"{not-json";
    fs::write(root.join("profile-notes.json"), corrupt).unwrap();
    assert!(
        store
            .save_note("8902".into(), ProfileNote::default())
            .is_err()
    );
    assert_eq!(fs::read(root.join("profile-notes.json")).unwrap(), corrupt);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn note_store_rejects_a_symlink_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let root = test_root();
    let outside = root.with_extension("outside.json");
    fs::write(&outside, br#"{"untouched":true}"#).unwrap();
    symlink(&outside, root.join("profile-notes.json")).unwrap();
    let store = Store::new(&root);
    assert!(store.load().is_err());
    assert!(
        store
            .save_note("8901".into(), ProfileNote::default())
            .is_err()
    );
    assert_eq!(fs::read(&outside).unwrap(), br#"{"untouched":true}"#);
    let _ = fs::remove_file(root.join("profile-notes.json"));
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_file(outside);
}

#[test]
fn profile_note_writes_are_serialized_across_store_clones() {
    let root = test_root();
    let store = Arc::new(Store::new(&root));
    let workers = (0..8)
        .map(|index| {
            let store = store.clone();
            thread::spawn(move || {
                store
                    .save_note(
                        format!("890{index}"),
                        ProfileNote {
                            label: format!("card-{index}"),
                            ..ProfileNote::default()
                        },
                    )
                    .unwrap();
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(store.load().unwrap().len(), 8);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn public_profile_notes_preserve_routes_validation_and_go_error_shapes() {
    let root = test_root();
    let store = Store::new(&root);
    let (status, value) = response_json(
        handle_public(
            full_request(Method::GET, "/api/esim/notes", ""),
            store.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, json!({"notes": {}}));
    assert!(
        !root.join("profile-notes.json.lock").exists(),
        "a read-only note snapshot must not create storage files"
    );

    let (status, value) = response_json(
        handle_public(
            full_request(
                Method::PUT,
                "/api/esim/notes",
                r#"{"iccid":"8901000000000000000","label":"卡卡","phone":"+44123","tags":"travel"}"#,
            ),
            store.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["message"], "本地备注已保存");
    assert_eq!(value["note"]["label"], "卡卡");

    let too_long = format!(
        r#"{{"iccid":"8901000000000000000","label":"{}","phone":"","tags":""}}"#,
        "卡".repeat(81)
    );
    let (status, value) = response_json(
        handle_public(
            full_request(Method::PUT, "/api/esim/notes", &too_long),
            store,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"], "本地备注字段过长");
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn private_profile_phone_bridge_is_method_scoped_and_preserves_other_fields() {
    let root = test_root();
    let store = Store::new(&root);
    let request = full_request(
        Method::POST,
        &format!("{INTERNAL_PREFIX}remember-phone"),
        r#"{"iccid":"8901000000000000000","phone":"+447700900123"}"#,
    );
    let (status, value) = response_json(handle_internal(request, store.clone()).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["ok"], true);
    assert!(
        store
            .save_note(
                "8901000000000000000".into(),
                ProfileNote {
                    label: "keep me".into(),
                    phone: "+447700900123".into(),
                    tags: "tag".into(),
                }
            )
            .is_ok()
    );
    let request = full_request(
        Method::POST,
        &format!("{INTERNAL_PREFIX}phone"),
        r#"{"iccid":"8901000000000000000F"}"#,
    );
    let (status, value) = response_json(handle_internal(request, store.clone()).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["phone"], "+447700900123");
    let wrong_method = full_request(Method::GET, &format!("{INTERNAL_PREFIX}phone"), "");
    assert_eq!(
        handle_internal(wrong_method, store).await.status(),
        StatusCode::NOT_FOUND
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn phone_and_iccid_normalization_match_existing_go_policy() {
    assert_eq!(
        normalize_sim_identifier(" \"8901234567890123456Ff\" "),
        "8901234567890123456"
    );
    for value in [
        "",
        " ",
        "-",
        "unknown",
        "号码未知",
        "未知号码",
        "N/A",
        "000000",
        "+000",
    ] {
        assert!(is_missing_phone(value), "{value:?} should be missing");
    }
    for value in ["+447700900123", "123456789"] {
        assert!(!is_missing_phone(value), "{value:?} should be usable");
    }
}

fn extension_fixture() -> Value {
    json!({
        "8901": {"label":"travel", "phone":"+44123", "tags":"work",
            "future": {"nested":[null, true, 18446744073709551615u64, "保持原值"], "enabled":false}},
        "8902": {"label":"other", "phone":"+8521234", "tags":"keep",
            "future": [1, {"name":"untouched"}], "optional":null}
    })
}

#[tokio::test]
async fn public_edits_and_clears_preserve_extensions_without_exposing_them_in_dtos() {
    let root = test_root();
    let path = root.join("profile-notes.json");
    let original = extension_fixture();
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    let store = Store::new(&root);
    for (label, phone, tags) in [("updated", "+44999", "new"), ("", "", "")] {
        let body = json!({"iccid":"8901", "label":label, "phone":phone, "tags":tags});
        let (status, response) = response_json(
            handle_public(
                full_request(Method::PUT, "/api/esim/notes", &body.to_string()),
                store.clone(),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            response["note"],
            json!({"label":label,"phone":phone,"tags":tags})
        );
        let persisted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["8901"]["future"], original["8901"]["future"]);
        assert_eq!(persisted["8901"]["label"], label);
        assert_eq!(persisted["8901"]["phone"], phone);
        assert_eq!(persisted["8901"]["tags"], tags);
        assert_eq!(persisted["8902"], original["8902"]);
        let (status, snapshot) = response_json(
            handle_public(
                full_request(Method::GET, "/api/esim/notes", ""),
                store.clone(),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(snapshot["notes"]["8901"], response["note"]);
        assert!(snapshot["notes"]["8902"].get("future").is_none());
    }
    let before = fs::read(&path).unwrap();
    let (status, _) = response_json(
        handle_public(
            full_request(
                Method::PUT,
                "/api/esim/notes",
                r#"{"iccid":"8901","future":"untrusted"}"#,
            ),
            store,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registered_phone_and_internal_phone_writers_preserve_other_versions_fields() {
    let root = test_root();
    let path = root.join("profile-notes.json");
    let original = extension_fixture();
    fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    let store = Store::new(&root);
    assert!(store.remember_phone("8901", "+44111").unwrap());
    assert_eq!(
        store.remember_registered_phone("\"8901F\"", "+44222", || true),
        Ok(Some(true))
    );
    assert_eq!(
        Store::new(&root)
            .registered_phone_fallback("8901F")
            .unwrap(),
        "+44222"
    );
    let persisted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted["8901"]["future"], original["8901"]["future"]);
    assert_eq!(persisted["8901"]["label"], original["8901"]["label"]);
    assert_eq!(persisted["8901"]["tags"], original["8901"]["tags"]);
    assert_eq!(persisted["8902"], original["8902"]);
    let unchanged = fs::read(&path).unwrap();
    assert_eq!(
        store.remember_registered_phone("8901", "+44333", || false),
        Ok(None)
    );
    assert_eq!(
        store.remember_registered_phone("8901", "+44222", || true),
        Ok(Some(false))
    );
    assert!(!store.remember_phone("8901", "+44222").unwrap());
    assert_eq!(fs::read(&path).unwrap(), unchanged);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unknown_fields_do_not_relax_invalid_known_fields_or_file_size_failures() {
    let root = test_root();
    let path = root.join("profile-notes.json");
    let store = Store::new(&root);
    for invalid in [
        r#"{"8901":{"phone":42,"future":{"opaque":true}}}"#,
        r#"{"8901":{"label":null,"future":[1,2]}}"#,
        r#"{"8901":null}"#,
    ] {
        fs::write(&path, invalid).unwrap();
        assert!(store.load().is_err());
        assert!(store.remember_phone("8902", "+44123").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
    }
    let skeleton = r#"{"8901":{"label":"","phone":"","tags":"","future":""}}"#;
    let future = "x".repeat(super::MAX_FILE_BYTES as usize - skeleton.len() - 4);
    let original =
        serde_json::to_vec(&json!({"8901":{"label":"","phone":"","tags":"","future":future}}))
            .unwrap();
    assert_eq!(original.len() as u64, super::MAX_FILE_BYTES - 4);
    fs::write(&path, &original).unwrap();
    assert_eq!(store.load().unwrap().len(), 1);
    assert!(
        store
            .save_note(
                "8901".into(),
                ProfileNote {
                    label: "edited".into(),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!fs::read_dir(&root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn opaque_extensions_preserve_exact_numbers_nested_json_and_future_field_names() {
    let root = test_root();
    let path = root.join("profile-notes.json");
    let large = "18446744073709551617";
    let precise = "0.123456789012345678901234567890123456789";
    let nested = r#"{ "labels": [ "x", 18446744073709551617 ], "phone": { "future": true } }"#;
    let original = format!(
        r#"{{"8901":{{"label":"before","phone":"","tags":"","huge":{large},"precise":{precise},"opaque":{nested},"$serde_json::private::RawValue":"keep"}}}}"#
    );
    fs::write(&path, original).unwrap();
    Store::new(&root)
        .save_note(
            "8901".into(),
            ProfileNote {
                label: "after".into(),
                ..Default::default()
            },
        )
        .unwrap();
    type RawMap = std::collections::BTreeMap<String, Box<serde_json::value::RawValue>>;
    let persisted = fs::read(&path).unwrap();
    let root_values: RawMap = serde_json::from_slice(&persisted).unwrap();
    let values: RawMap = serde_json::from_str(root_values["8901"].get()).unwrap();
    assert_eq!(values["huge"].get(), large);
    assert_eq!(values["precise"].get(), precise);
    assert_eq!(values["opaque"].get(), nested);
    assert_eq!(values["$serde_json::private::RawValue"].get(), r#""keep""#);
    assert_eq!(Store::new(&root).load().unwrap()["8901"].label, "after");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_duplicate_object_keys_keep_the_last_value_without_losing_raw_extensions() {
    let root = test_root();
    let path = root.join("profile-notes.json");
    fs::write(&path, r#"{"8901":{"label":"discarded"},"8901":{"label":null,"label":"last","phone":"+44123","future":false,"future":18446744073709551617}}"#).unwrap();
    let store = Store::new(&root);
    assert_eq!(store.load().unwrap()["8901"].label, "last");
    assert!(store.remember_phone("8901", "+44222").unwrap());
    let raw = fs::read_to_string(&path).unwrap();
    assert!(raw.contains("18446744073709551617"));
    assert_eq!(store.load().unwrap()["8901"].phone, "+44222");
    assert_eq!(store.load().unwrap()["8901"].label, "last");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn stalled_note_body_expires_before_any_store_write() {
    struct StalledBody;
    impl hyper::body::Body for StalledBody {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>)
            -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            std::task::Poll::Pending
        }
    }
    let root = test_root();
    let request = Request::builder().method(Method::PUT).uri("/api/esim/notes")
        .body(StalledBody).unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(7),
        handle_public(request, Store::new(&root))).await
        .expect("unfinished ingress must not occupy a request indefinitely");
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert!(!root.join("profile-notes.json").exists());
    assert!(Store::new(&root).load().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}
