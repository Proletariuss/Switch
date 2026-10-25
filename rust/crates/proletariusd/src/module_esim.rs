//! Rust-owned module eUICC operations over the selected serialized native AT owner.
//!
//! Profile reads reuse the same ES10c implementation as the reader eSIM path;
//! this module only handles modem logical-channel framing and the legacy
//! overview DTO.

use std::{
    collections::HashSet,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use bytes::Bytes;
use http_body_util::Full;
use hyper::StatusCode;
use hyper::{Request, Response, body::Incoming};
use proletarius_euicc::{
    DEFAULT_MSS, EID_LENGTH_BYTES, Euicc, KNOWN_ISDR_APPLICATION_AIDS, MAX_PROFILE_COUNT,
    ProductField, ProfileIdentifier, ProfileInfo, ProfileOperationResult, ProfileState,
    encode_iccid,
};
use proletarius_lpa::HttpEs9Client;
use proletarius_reader::euicc_transport::ApduCardTransport;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use zeroize::Zeroize;

use crate::module_esim_channel::{AtCommandTransport, Error as ChannelError, ModuleEuiccChannel};
use crate::{reader_esim_download, usb_at};

mod vowifi;
pub(crate) use vowifi::stop_module_vowifi_before_write;

const PROFILE_READ_ERROR: &str = "Profile 列表读取失败，请刷新后重试";
const MAX_RENAME_BODY_BYTES: usize = 1 << 20;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RenameProfileRequest {
    client_request_id: String,
    iccid: String,
    #[serde(default)]
    aid: String,
    name: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeleteProfileRequest {
    client_request_id: String,
    #[serde(default)]
    iccid: String,
    #[serde(default)]
    aid: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct SwitchProfileRequest {
    client_request_id: String,
    #[serde(default)]
    iccid: String,
    #[serde(default)]
    aid: String,
}

#[derive(Debug)]
struct ModuleSwitchFailure {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    diagnostic: &'static str,
}

#[derive(Debug)]
struct ModuleSwitchAccepted {
    warning: Option<(String, String)>,
}

struct ModuleSwitchHttpFailure {
    status: StatusCode,
    code: &'static str,
    message: String,
    recoverable: bool,
}

/// Execute the selected native module eSIM switch in Rust.
pub(crate) async fn switch_profile(
    request: Request<Incoming>,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    if !crate::module_esim_transport::native(state) {
        return module_unavailable();
    }

    let body = match decode_switch_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let intent = match crate::esim_mutation_intent::module_request(
        state,
        &body.client_request_id,
        "switch",
        &body,
    ) {
        Ok(intent) => intent,
        Err(response) => return response,
    };
    let state = state.clone();
    // Dropping the HTTP response future detaches this job; RF cleanup and the
    // physical channel transaction must finish before operation admission ends.
    match tokio::spawn(async move {
        crate::esim_mutation_intent::execute(intent, || switch_profile_native(body, state)).await
    })
    .await
    {
        Ok(response) => response,
        Err(_) => crate::json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"error":"切卡结果待确认，请刷新操作状态后再操作", "code":"switch_operation_state_unavailable", "recoverable":true}),
        ),
    }
}

async fn switch_profile_native(
    body: SwitchProfileRequest,
    owned_state: crate::ServerState,
) -> Response<Full<Bytes>> {
    let state = &owned_state;
    let iccid = body.iccid.trim().to_owned();
    if iccid.is_empty() {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "error":"请选择要启用的 Profile",
                "code":"invalid_iccid",
                "recoverable":false
            }),
        );
    }
    if encode_iccid(&iccid).is_err() {
        return crate::json_response(
            StatusCode::BAD_GATEWAY,
            &json!({
                "error":"切换 Profile 失败：ICCID 格式无效；请刷新卡片状态后重试",
                "code":"profile_switch_failed",
                "recoverable":true
            }),
        );
    }
    let aid = match decode_aid(&body.aid) {
        Some(aid) => aid,
        None => {
            return crate::json_response(
                StatusCode::BAD_GATEWAY,
                &json!({
                    "error":"切换 Profile 失败：AID 格式无效；请刷新卡片状态后重试",
                    "code":"profile_switch_failed",
                    "recoverable":true
                }),
            );
        }
    };

    let operation = match reader_esim_download::begin_module_switch(&iccid) {
        Ok(operation) => Arc::new(operation),
        Err(error) => return module_operation_admission_error(error),
    };
    if let Err(error) = stop_module_vowifi_before_write(state).await {
        reader_esim_download::fail_module_switch(
            operation,
            error.code,
            error.message,
            "native module IMS/RF retirement rejected profile switch before APDU submission",
        );
        return error.response();
    }
    let reservation = match state.module_radio.reserve_esim().await {
        Ok(reservation) => reservation,
        Err(error) => {
            let code = error.public_code();
            let message = error.public_message().to_owned();
            reader_esim_download::fail_module_switch(
                operation,
                code,
                &message,
                "module eSIM radio reservation failed before APDU submission",
            );
            return error.module_radio_compat_response();
        }
    };
    if !reader_esim_download::update_module_switch(
        &operation,
        "switch",
        "正在向 eUICC 提交 Profile 切换…",
        25,
    ) {
        let _ = state.module_radio.release_esim(reservation).await;
        reader_esim_download::fail_module_switch(
            operation,
            "esim_operation_journal_failed",
            "无法安全记录切卡状态，尚未操作 eUICC",
            "module eSIM switch journal update failed before APDU submission",
        );
        return crate::json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({
                "error":"无法安全记录切卡状态，尚未操作 eUICC",
                "code":"esim_operation_journal_failed",
                "recoverable":true
            }),
        );
    }

    let (response_sender, response_receiver) = tokio::sync::oneshot::channel();
    let worker_state = state.clone();
    let worker_operation = operation.clone();
    tokio::spawn(async move {
        run_module_switch(
            worker_state,
            worker_operation,
            reservation,
            iccid,
            aid,
            response_sender,
        )
        .await;
    });
    drop(operation);

    match response_receiver.await {
        Ok(Ok(response)) => crate::json_response(StatusCode::OK, &response),
        Ok(Err(error)) => crate::json_response(
            error.status,
            &json!({
                "error":error.message,
                "code":error.code,
                "recoverable":error.recoverable
            }),
        ),
        Err(_) => crate::json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({
                "error":"切卡结果待确认，请刷新操作状态后再操作",
                "code":"switch_operation_state_unavailable",
                "recoverable":true
            }),
        ),
    }
}

fn module_unavailable() -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &json!({"error":"eSIM 管理服务当前不可用","code":"esim_unavailable","retryable":true}),
    )
}

async fn run_module_switch(
    state: crate::ServerState,
    operation: Arc<reader_esim_download::ModuleSwitchOperation>,
    reservation: crate::module_radio::EsimReservation,
    iccid: String,
    aid: Option<Vec<u8>>,
    response_sender: tokio::sync::oneshot::Sender<Result<Value, ModuleSwitchHttpFailure>>,
) {
    let switch_fence = crate::module_esim_transport::mutation_fence(&state);
    let operation_worker = operation.clone();
    let worker_iccid = iccid.clone();
    let worker_result =
        crate::module_esim_transport::transaction_fenced(&state, &switch_fence, move |transport| {
            let result = switch_profile_on_transport_with_wait(
                transport,
                &worker_iccid,
                aid.as_deref(),
                |eid, selected_aid| {
                    reader_esim_download::arm_module_switch_proof(
                        &operation_worker,
                        eid,
                        selected_aid,
                    )
                },
                std::thread::sleep,
            );
            drop(operation_worker);
            Ok(result)
        })
        .await;

    let accepted = match worker_result {
        Ok(Ok(accepted)) => accepted,
        Ok(Err(error)) => {
            finish_module_switch_failure(&state, operation, reservation, error, response_sender)
                .await;
            return;
        }
        Err(_) => {
            finish_module_switch_failure(
                &state,
                operation,
                reservation,
                ModuleSwitchFailure {
                    status: StatusCode::BAD_GATEWAY,
                    code: "profile_switch_failed",
                    message: "切换 Profile 未完成；请刷新卡片状态确认当前启用的号卡，不要连续重复切卡",
                    diagnostic: "native AT transaction failed before switch completion",
                },
                response_sender,
            )
            .await;
            return;
        }
    };

    // Match the existing module workflow's short card-settling delay before
    // moving into the bounded 90-second modem identity recovery window.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = reader_esim_download::update_module_switch(
        &operation,
        "module_refresh",
        "Profile 已受理，正在同步模块状态…",
        70,
    );
    let operation_snapshot = reader_esim_download::current_operation_json();
    let strategy = if accepted.warning.is_some() {
        "readback_confirmed_then_check_module_without_reboot"
    } else {
        "check_then_radio_function_cycle_no_reboot"
    };
    let _ = response_sender.send(Ok(json!({
        "switch_accepted":true,
        "phase":"done",
        "target_iccid":iccid,
        "recovery_pending":true,
        "module_reboot_requested":false,
        "module_refresh_strategy":strategy,
        "reconnect_wait_seconds":90,
        "operation":operation_snapshot
    })));

    monitor_module_switch(
        state,
        operation,
        reservation,
        iccid,
        accepted.warning,
        switch_fence,
    )
    .await;
}

async fn finish_module_switch_failure(
    state: &crate::ServerState,
    operation: Arc<reader_esim_download::ModuleSwitchOperation>,
    reservation: crate::module_radio::EsimReservation,
    error: ModuleSwitchFailure,
    response_sender: tokio::sync::oneshot::Sender<Result<Value, ModuleSwitchHttpFailure>>,
) {
    let release_failed = state.module_radio.release_esim(reservation).await.is_err();
    let message = if release_failed {
        format!("{}；模块射频操作权释放未确认", error.message)
    } else {
        error.message.to_owned()
    };
    reader_esim_download::fail_module_switch(operation, error.code, &message, error.diagnostic);
    let _ = response_sender.send(Err(ModuleSwitchHttpFailure {
        status: error.status,
        code: error.code,
        message,
        recoverable: true,
    }));
}

async fn monitor_module_switch(
    state: crate::ServerState,
    operation: Arc<reader_esim_download::ModuleSwitchOperation>,
    reservation: crate::module_radio::EsimReservation,
    target_iccid: String,
    initial_warning: Option<(String, String)>,
    switch_fence: crate::module_esim_transport::MutationFence,
) {
    tokio::time::sleep(Duration::from_secs(3)).await;
    let _ = reader_esim_download::update_module_switch(
        &operation,
        "module_refresh",
        "正在刷新模块中的 SIM 网络状态…",
        76,
    );

    let mut confirmed = switch_fence.same_transport(&state)
        && module_profile_matches(&state, &switch_fence, &target_iccid).await;
    if !confirmed && switch_fence.same_transport(&state) {
        // The profile switch may already have propagated. Only issue the
        // existing light CFUN refresh after a read-only ICCID check, and never
        // reboot the module as a fallback.
        let _ = state
            .module_radio
            .refresh_esim_profile(&reservation, operation.id())
            .await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if !switch_fence.same_transport(&state) {
                break;
            }
            if module_profile_matches(&state, &switch_fence, &target_iccid).await {
                confirmed = true;
                break;
            }
        }
    }

    let refresh_fence = crate::module_esim_transport::mutation_fence(&state);
    let refreshed = if confirmed && switch_fence.same_transport(&state) {
        crate::module_esim_transport::refresh_reserved(&state, &refresh_fence, &reservation).await
    } else {
        None
    };
    let release_failed = state.module_radio.release_esim(reservation).await.is_err();
    if release_failed {
        crate::module_esim_transport::discard_unreleased_refresh(&state, refreshed.as_ref());
    }
    let warning = if release_failed {
        Some((
            "目标 Profile 状态已检查，但模块射频操作权释放未确认；请刷新模块状态后再继续操作"
                .into(),
            "module_radio_release_unconfirmed".into(),
        ))
    } else if !confirmed {
        Some((
            "模块尚未确认新 Profile；应用未重启模块。请检查读卡器/模块连接后刷新状态".into(),
            "switch_profile_refresh_unconfirmed".into(),
        ))
    } else {
        initial_warning
    };
    // Terminal state includes the final cache read and physical release.
    reader_esim_download::finish_module_switch(operation, warning);
}

async fn module_profile_matches(
    state: &crate::ServerState,
    fence: &crate::module_esim_transport::MutationFence,
    target: &str,
) -> bool {
    let target = normalize_module_iccid(target);
    if target.is_empty() {
        return false;
    }
    crate::module_esim_transport::observe_same_transport(state, fence, move |transport| {
        let response_result = transport.command("AT+QCCID", Duration::from_secs(3), 1024);
        let matches = response_result.map(|mut response| {
            let observed = parse_module_iccid(&response);
            response.zeroize();
            observed.is_some_and(|mut observed| {
                let matches = normalize_module_iccid(&observed) == target;
                observed.zeroize();
                matches
            })
        })?;
        Ok(matches)
    })
    .await
    .unwrap_or(false)
}

fn normalize_module_iccid(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_end_matches(['F', 'f'])
        .to_owned()
}

fn parse_module_iccid(response: &str) -> Option<String> {
    let mut found = None;
    for line in response.lines().map(str::trim) {
        let Some(value) = line.strip_prefix("+QCCID:") else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        let value = value.trim().trim_matches('"');
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'F' || byte == b'f')
        {
            return None;
        }
        found = Some(value.to_owned());
    }
    found
}

/// Rename a profile through the selected native module owner.
pub(crate) async fn rename_profile(
    request: Request<Incoming>,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    if !crate::module_esim_transport::native(state) {
        return module_unavailable();
    }

    let body = match decode_rename_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let intent = match crate::esim_mutation_intent::module_request(
        state,
        &body.client_request_id,
        "rename",
        &body,
    ) {
        Ok(intent) => intent,
        Err(response) => return response,
    };
    let state = state.clone();
    // Dropping the HTTP response future detaches this job; RF cleanup and the
    // physical channel transaction must finish before operation admission ends.
    match tokio::spawn(async move {
        crate::esim_mutation_intent::execute(intent, || rename_profile_native(body, state)).await
    })
    .await
    {
        Ok(response) => response,
        Err(_) => rename_failed(),
    }
}

async fn rename_profile_native(
    body: RenameProfileRequest,
    owned_state: crate::ServerState,
) -> Response<Full<Bytes>> {
    let state = &owned_state;
    let iccid = body.iccid.trim().to_owned();
    let name = body.name.trim().to_owned();
    if iccid.is_empty() || name.is_empty() {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "error":"Profile 名称不能为空",
                "code":"invalid_profile_name",
                "recoverable":false
            }),
        );
    }
    if name.chars().count() > 64 {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "error":"Profile 名称不能超过 64 个字符",
                "code":"invalid_profile_name",
                "recoverable":false
            }),
        );
    }
    let aid = match decode_aid(&body.aid) {
        Some(aid) => aid,
        None => {
            return rename_failed();
        }
    };

    let operation_gate = match crate::reader_esim_download::begin_module_operation() {
        Ok(permit) => std::sync::Arc::new(permit),
        Err(error) => return module_operation_admission_error(error),
    };
    if let Err(error) = stop_module_vowifi_before_write(state).await {
        return error.response();
    }
    let reservation = match state.module_radio.reserve_esim().await {
        Ok(reservation) => reservation,
        Err(error) => return error.module_radio_compat_response(),
    };
    let refresh_fence = crate::module_esim_transport::mutation_fence(state);
    let worker_operation_gate = operation_gate.clone();
    let result =
        crate::module_esim_transport::transaction_fenced(state, &refresh_fence, move |transport| {
            let result = rename_profile_on_transport(transport, &iccid, aid.as_deref(), &name);
            drop(worker_operation_gate);
            result
        })
        .await;
    let refreshed = if result.is_ok() {
        crate::module_esim_transport::refresh_reserved(state, &refresh_fence, &reservation).await
    } else {
        None
    };
    let release = state.module_radio.release_esim(reservation).await;
    if release.is_err() {
        crate::module_esim_transport::discard_unreleased_refresh(state, refreshed.as_ref());
    }

    if result.is_err() {
        return rename_failed();
    }
    if let Err(error) = release {
        return error.module_radio_compat_response();
    }

    // Admission includes the final cache read and actual RF cleanup.
    drop(operation_gate);
    crate::json_response(StatusCode::OK, &json!({"message":"Profile 名称修改成功"}))
}

/// Delete a profile through the selected native module owner.
pub(crate) async fn delete_profile(
    request: Request<Incoming>,
    state: &crate::ServerState,
) -> Response<Full<Bytes>> {
    if !crate::module_esim_transport::native(state) {
        return module_unavailable();
    }

    let body = match decode_delete_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let intent = match crate::esim_mutation_intent::module_request(
        state,
        &body.client_request_id,
        "delete",
        &body,
    ) {
        Ok(intent) => intent,
        Err(response) => return response,
    };
    let state = state.clone();
    // Dropping the HTTP response future detaches this job; RF cleanup and the
    // physical channel transaction must finish before operation admission ends.
    match tokio::spawn(async move {
        crate::esim_mutation_intent::execute(intent, || delete_profile_native(body, state)).await
    })
    .await
    {
        Ok(response) => response,
        Err(_) => delete_profile_failed(),
    }
}

async fn delete_profile_native(
    body: DeleteProfileRequest,
    owned_state: crate::ServerState,
) -> Response<Full<Bytes>> {
    let state = &owned_state;
    let iccid = body.iccid.trim().to_owned();
    if proletarius_euicc::encode_iccid(&iccid).is_err() {
        return crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({
                "error":"请选择要删除的 Profile",
                "code":"invalid_iccid",
                "recoverable":false
            }),
        );
    }
    let aid = match decode_aid(&body.aid) {
        Some(aid) => aid,
        None => {
            return crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({
                    "error":"AID 格式无效",
                    "code":"invalid_aid_hex",
                    "recoverable":false
                }),
            );
        }
    };

    // The Rust operation gate serializes module writes with reader downloads
    // and switches, including unresolved journal outcomes.
    let operation_gate = match crate::reader_esim_download::begin_module_operation() {
        Ok(permit) => std::sync::Arc::new(permit),
        Err(error) => return module_operation_admission_error(error),
    };

    if let Err(error) = stop_module_vowifi_before_write(state).await {
        return error.response();
    }
    let reservation = match state.module_radio.reserve_esim().await {
        Ok(reservation) => reservation,
        Err(error) => return error.module_radio_compat_response(),
    };
    let refresh_fence = crate::module_esim_transport::mutation_fence(state);
    let worker_operation_gate = operation_gate.clone();
    let result =
        crate::module_esim_transport::transaction_fenced(state, &refresh_fence, move |transport| {
            let result = Ok(delete_profile_on_transport(
                transport,
                &iccid,
                aid.as_deref(),
            ));
            drop(worker_operation_gate);
            result
        })
        .await;
    let refreshed = if matches!(&result, Ok(Ok(Some(_)))) {
        crate::module_esim_transport::refresh_reserved(state, &refresh_fence, &reservation).await
    } else {
        None
    };
    let release = state.module_radio.release_esim(reservation).await;
    if release.is_err() {
        crate::module_esim_transport::discard_unreleased_refresh(state, refreshed.as_ref());
    }

    match result {
        Ok(Ok(Some(result))) => {
            if let Err(error) = release {
                return error.module_radio_compat_response();
            }
            drop(operation_gate);
            crate::json_response(StatusCode::OK, &result)
        }
        Ok(Ok(None)) => {
            if let Err(error) = release {
                return error.module_radio_compat_response();
            }
            crate::json_response(
                StatusCode::NOT_FOUND,
                &json!({
                    "error":"未找到指定的 eSIM Profile",
                    "code":"profile_not_found",
                    "recoverable":false
                }),
            )
        }
        Ok(Err(ModuleDeleteFailure::EuiccNotFound)) => {
            if let Err(error) = release {
                return error.module_radio_compat_response();
            }
            crate::json_response(
                StatusCode::NOT_FOUND,
                &json!({
                    "error":"未找到可访问的 eUICC",
                    "code":"euicc_not_found",
                    "recoverable":false
                }),
            )
        }
        Ok(Err(ModuleDeleteFailure::Operation)) | Err(_) => {
            let _ = release;
            delete_profile_failed()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModuleDeleteFailure {
    EuiccNotFound,
    Operation,
}

fn delete_profile_on_transport(
    transport: &mut dyn AtCommandTransport<Error = usb_at::Failure>,
    iccid: &str,
    requested_aid: Option<&[u8]>,
) -> Result<Option<reader_esim_download::DeleteProfileResult>, ModuleDeleteFailure> {
    let network = HttpEs9Client::new().map_err(|_| ModuleDeleteFailure::Operation)?;
    let candidates =
        requested_aid.map_or_else(|| KNOWN_ISDR_APPLICATION_AIDS.to_vec(), |aid| vec![aid]);
    let mut euicc_found = false;

    for aid in candidates {
        let channel = match ModuleEuiccChannel::open(transport, aid) {
            Ok(channel) => channel,
            Err(ChannelError::ModemRejected) if requested_aid.is_none() => continue,
            Err(ChannelError::ModemRejected) => {
                return Err(ModuleDeleteFailure::EuiccNotFound);
            }
            Err(_) => return Err(ModuleDeleteFailure::Operation),
        };
        euicc_found = true;
        let Some(channel_number) = channel.channel() else {
            let mut channel = channel;
            let _ = channel.close();
            return Err(ModuleDeleteFailure::Operation);
        };
        let mut card = Euicc::new(ApduCardTransport::new(channel), channel_number, DEFAULT_MSS)
            .map_err(|_| ModuleDeleteFailure::Operation)?;
        let operation = reader_esim_download::delete_profile_on_euicc(
            &mut card,
            &network,
            iccid,
            &AtomicBool::new(false),
        );
        let mut channel = card.into_inner().into_inner();
        let close = channel.close();
        drop(channel);

        match (operation, close) {
            (Ok(Some(mut result)), Err(_)) => {
                result.warning = Some("Profile 已删除，但模块逻辑通道关闭未确认".into());
                result.warning_code = Some("module_channel_close_failed".into());
                return Ok(Some(result));
            }
            (Ok(Some(result)), Ok(())) => return Ok(Some(result)),
            (Ok(None), Ok(())) => continue,
            (Ok(None), Err(_)) | (Err(()), _) => {
                return Err(ModuleDeleteFailure::Operation);
            }
        }
    }

    if euicc_found {
        Ok(None)
    } else {
        Err(ModuleDeleteFailure::EuiccNotFound)
    }
}

async fn decode_switch_body(
    request: Request<Incoming>,
) -> Result<SwitchProfileRequest, Response<Full<Bytes>>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        MAX_RENAME_BODY_BYTES,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => body,
        Err(_) => {
            return Err(crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"invalid request body"}),
            ));
        }
    };
    let mut deserializer = serde_json::Deserializer::from_slice(&body);
    let parsed = SwitchProfileRequest::deserialize(&mut deserializer);
    if parsed.is_err() || deserializer.end().is_err() {
        return Err(crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"invalid request body"}),
        ));
    }
    parsed.map_err(|_| {
        crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"invalid request body"}),
        )
    })
}

async fn decode_rename_body(
    request: Request<Incoming>,
) -> Result<RenameProfileRequest, Response<Full<Bytes>>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        MAX_RENAME_BODY_BYTES,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => body,
        Err(_) => {
            return Err(crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"invalid request body"}),
            ));
        }
    };
    let mut deserializer = serde_json::Deserializer::from_slice(&body);
    let parsed = RenameProfileRequest::deserialize(&mut deserializer);
    if parsed.is_err() || deserializer.end().is_err() {
        return Err(crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"invalid request body"}),
        ));
    }
    parsed.map_err(|_| {
        crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"invalid request body"}),
        )
    })
}

async fn decode_delete_body(
    request: Request<Incoming>,
) -> Result<DeleteProfileRequest, Response<Full<Bytes>>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        MAX_RENAME_BODY_BYTES,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(body) => body,
        Err(_) => {
            return Err(crate::json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error":"invalid request body"}),
            ));
        }
    };
    let mut deserializer = serde_json::Deserializer::from_slice(&body);
    let parsed = DeleteProfileRequest::deserialize(&mut deserializer);
    if parsed.is_err() || deserializer.end().is_err() {
        return Err(crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"invalid request body"}),
        ));
    }
    parsed.map_err(|_| {
        crate::json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error":"invalid request body"}),
        )
    })
}

fn esim_operation_busy() -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::CONFLICT,
        &json!({
            "error":"已有 eSIM 操作正在进行，请稍后重试",
            "code":"esim_operation_busy",
            "recoverable":true
        }),
    )
}

fn module_operation_admission_error(
    error: crate::reader_esim_download::ModuleOperationAdmissionError,
) -> Response<Full<Bytes>> {
    match error {
        crate::reader_esim_download::ModuleOperationAdmissionError::Busy => esim_operation_busy(),
        crate::reader_esim_download::ModuleOperationAdmissionError::UnsafeState => {
            crate::json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({
                    "error":"无法安全协调 eSIM 操作，操作未开始",
                    "code":"esim_operation_journal_failed",
                    "retryable":true
                }),
            )
        }
    }
}

fn rename_failed() -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::BAD_GATEWAY,
        &json!({
            "error":"修改 Profile 名称失败，请稍后重试",
            "code":"rename_profile_failed",
            "recoverable":true
        }),
    )
}

fn delete_profile_failed() -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::BAD_GATEWAY,
        &json!({
            "error":"删除 Profile 失败，请刷新卡片状态后重试",
            "code":"delete_profile_failed",
            "recoverable":true
        }),
    )
}

fn decode_aid(input: &str) -> Option<Option<Vec<u8>>> {
    let input = input.trim();
    if input.is_empty() {
        return Some(None);
    }
    if input.len() > 64 || input.len() % 2 != 0 {
        return None;
    }
    let mut output = Vec::with_capacity(input.len() / 2);
    for pair in input.as_bytes().chunks_exact(2) {
        output.push((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?);
    }
    Some(Some(output))
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn switch_profile_on_transport_with_wait(
    transport: &mut dyn AtCommandTransport<Error = usb_at::Failure>,
    iccid: &str,
    requested_aid: Option<&[u8]>,
    mut arm_proof: impl FnMut(&[u8], &[u8]) -> Result<(), &'static str>,
    mut wait_before_retry: impl FnMut(Duration),
) -> Result<ModuleSwitchAccepted, ModuleSwitchFailure> {
    let encoded_iccid = encode_iccid(iccid).map_err(|_| ModuleSwitchFailure {
        status: StatusCode::BAD_GATEWAY,
        code: "profile_switch_failed",
        message: "切换 Profile 失败：ICCID 格式无效；请刷新卡片状态后重试",
        diagnostic: "target ICCID failed local SGP.22 validation",
    })?;
    let candidates: Vec<&[u8]> = requested_aid
        .map(|aid| vec![aid])
        .unwrap_or_else(|| KNOWN_ISDR_APPLICATION_AIDS.to_vec());
    let mut euicc_opened = false;

    for aid in candidates {
        let channel = match ModuleEuiccChannel::open(transport, aid) {
            Ok(channel) => channel,
            Err(ChannelError::ModemRejected) if requested_aid.is_none() => continue,
            Err(ChannelError::ModemRejected) => {
                return Err(ModuleSwitchFailure {
                    status: StatusCode::BAD_GATEWAY,
                    code: "profile_switch_failed",
                    message: "切换 Profile 未完成；请刷新卡片状态确认当前启用的号卡，不要连续重复切卡",
                    diagnostic: "modem rejected the requested ISD-R AID",
                });
            }
            Err(_) => {
                return Err(ModuleSwitchFailure {
                    status: StatusCode::BAD_GATEWAY,
                    code: "profile_switch_failed",
                    message: "切换 Profile 未完成；请刷新卡片状态确认当前启用的号卡，不要连续重复切卡",
                    diagnostic: "could not open the module eUICC logical channel",
                });
            }
        };
        euicc_opened = true;
        let channel_number = match channel.channel() {
            Some(channel_number) => channel_number,
            None => {
                let mut channel = channel;
                let _ = channel.close();
                return Err(ModuleSwitchFailure {
                    status: StatusCode::BAD_GATEWAY,
                    code: "profile_switch_failed",
                    message: "切换 Profile 未完成；请刷新卡片状态确认当前启用的号卡，不要连续重复切卡",
                    diagnostic: "module eUICC channel did not provide a logical channel",
                });
            }
        };
        let mut card = match Euicc::new(
            ApduCardTransport::new(channel),
            channel_number,
            DEFAULT_MSS,
        ) {
            Ok(card) => card,
            Err(_) => {
                return Err(ModuleSwitchFailure {
                    status: StatusCode::BAD_GATEWAY,
                    code: "profile_switch_failed",
                    message: "切换 Profile 未完成；请刷新卡片状态确认当前启用的号卡，不要连续重复切卡",
                    diagnostic: "could not initialize the module eUICC session",
                });
            }
        };

        let mut profiles = vec![ProfileInfo::default(); MAX_PROFILE_COUNT];
        let profile_count = match card.list_profiles_into(&mut profiles) {
            Ok(count) => count,
            Err(_) => {
                let mut channel = card.into_inner().into_inner();
                let _ = channel.close();
                return Err(ModuleSwitchFailure {
                    status: StatusCode::BAD_GATEWAY,
                    code: "profile_switch_failed",
                    message: "切换 Profile 失败；无法读取 eUICC 状态，请刷新卡片状态后重试",
                    diagnostic: "eUICC profile-list read failed; the switch command was not sent",
                });
            }
        };
        let target = profiles[..profile_count]
            .iter()
            .find(|profile| profile.iccid_string().as_deref() == Ok(iccid));
        let Some(target) = target else {
            let mut channel = card.into_inner().into_inner();
            let close = channel.close();
            drop(channel);
            if close.is_err() {
                return Err(ModuleSwitchFailure {
                    status: StatusCode::BAD_GATEWAY,
                    code: "profile_switch_failed",
                    message: "切换 Profile 未完成；eUICC 通道状态未能确认，请刷新后重试",
                    diagnostic: "logical channel close failed after profile lookup",
                });
            }
            if requested_aid.is_some() {
                return Err(ModuleSwitchFailure {
                    status: StatusCode::NOT_FOUND,
                    code: "profile_switch_target_missing",
                    message: "卡片未找到目标 Profile，请刷新列表后重新选择",
                    diagnostic: "target ICCID is absent from the requested eUICC",
                });
            }
            continue;
        };

        if target.state == ProfileState::Enabled {
            let mut channel = card.into_inner().into_inner();
            let close = channel.close();
            drop(channel);
            return if close.is_ok() {
                Ok(ModuleSwitchAccepted { warning: None })
            } else {
                Ok(ModuleSwitchAccepted {
                    warning: Some((
                        "目标 Profile 已处于启用状态，但模块逻辑通道关闭未确认；正在核对模块状态"
                            .into(),
                        "switch_profile_response_uncertain".into(),
                    )),
                })
            };
        }

        // The journal must identify this exact eUICC before EnableProfile can
        // reach the modem. GetEID and EnableProfile use the same open channel;
        // a missing/invalid EID or an uncommitted proof fails closed.
        let mut eid = [0_u8; EID_LENGTH_BYTES];
        let proof_result = match card.get_eid(&mut eid) {
            Ok(length)
                if length == EID_LENGTH_BYTES
                    && eid.iter().all(|byte| byte >> 4 <= 9 && byte & 0x0f <= 9) =>
            {
                arm_proof(&eid, aid).map_err(|_| "proof_unavailable")
            }
            _ => Err("eid_unconfirmed"),
        };
        eid.zeroize();
        if let Err(reason) = proof_result {
            let mut channel = card.into_inner().into_inner();
            let _ = channel.close();
            return Err(if reason == "proof_unavailable" {
                ModuleSwitchFailure {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    code: "esim_operation_proof_unavailable",
                    message: "无法安全记录卡片身份，Profile 尚未切换",
                    diagnostic: "module switch card proof was not committed before EnableProfile",
                }
            } else {
                ModuleSwitchFailure {
                    status: StatusCode::BAD_GATEWAY,
                    code: "profile_switch_identity_unconfirmed",
                    message: "无法核对 eUICC 身份，Profile 尚未切换",
                    diagnostic: "module switch GetEID was unconfirmed before EnableProfile",
                }
            });
        }

        let mut last_enable_error = None;
        const MAX_CAT_BUSY_RETRIES: usize = 3;
        for attempt in 0..=MAX_CAT_BUSY_RETRIES {
            match card.enable_profile_command(ProfileIdentifier::Iccid(&encoded_iccid), false) {
                Ok(()) => {
                    last_enable_error = None;
                    break;
                }
                Err(error)
                    if matches!(
                        &error,
                        proletarius_euicc::Error::ProfileOperation {
                            result: ProfileOperationResult::CatBusy,
                            ..
                        }
                    ) && attempt < MAX_CAT_BUSY_RETRIES =>
                {
                    last_enable_error = Some(error);
                    wait_before_retry(Duration::from_millis(800));
                }
                Err(error) => {
                    last_enable_error = Some(error);
                    break;
                }
            }
        }

        // EnableProfile is not idempotent after an uncertain transport
        // response. Close the submitted channel, then read the active profile
        // once through a fresh channel; never replay the mutation.
        let switch_error = last_enable_error;
        let mut channel = card.into_inner().into_inner();
        let close = channel.close();
        drop(channel);

        let readback_confirmed = if switch_error.is_some() {
            wait_before_retry(Duration::from_millis(200));
            module_profile_enabled_on_transport(transport, aid, iccid)
        } else {
            false
        };
        if readback_confirmed {
            return Ok(ModuleSwitchAccepted {
                warning: Some((
                    "eUICC 回读已确认目标 Profile 已启用；切换指令响应异常，正在同步模块状态。"
                        .into(),
                    "switch_profile_response_uncertain".into(),
                )),
            });
        }
        if let Some(error) = switch_error {
            let mut failure = classify_module_switch_error(&error);
            if close.is_err() {
                failure.diagnostic = "EnableProfile failed, readback did not confirm the target, and logical-channel close was not confirmed";
            }
            return Err(failure);
        }
        return if close.is_ok() {
            Ok(ModuleSwitchAccepted { warning: None })
        } else {
            Ok(ModuleSwitchAccepted {
                warning: Some((
                    "切换指令已提交，但模块逻辑通道关闭未确认；正在核对模块状态".into(),
                    "switch_profile_response_uncertain".into(),
                )),
            })
        };
    }

    Err(ModuleSwitchFailure {
        status: if euicc_opened {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_GATEWAY
        },
        code: if euicc_opened {
            "profile_switch_target_missing"
        } else {
            "profile_switch_failed"
        },
        message: if euicc_opened {
            "卡片未找到目标 Profile，请刷新列表后重新选择"
        } else {
            "切换 Profile 未完成；请刷新卡片状态确认当前启用的号卡，不要连续重复切卡"
        },
        diagnostic: if euicc_opened {
            "target ICCID is absent from every accessible ISD-R application"
        } else {
            "no module eUICC ISD-R application was accessible"
        },
    })
}

fn module_profile_enabled_on_transport(
    transport: &mut dyn AtCommandTransport<Error = usb_at::Failure>,
    aid: &[u8],
    iccid: &str,
) -> bool {
    let channel = match ModuleEuiccChannel::open(transport, aid) {
        Ok(channel) => channel,
        Err(_) => return false,
    };
    let Some(channel_number) = channel.channel() else {
        let mut channel = channel;
        let _ = channel.close();
        return false;
    };
    let Ok(mut card) = Euicc::new(ApduCardTransport::new(channel), channel_number, DEFAULT_MSS)
    else {
        return false;
    };
    let mut profiles = vec![ProfileInfo::default(); MAX_PROFILE_COUNT];
    let confirmed = card
        .list_profiles_into(&mut profiles)
        .ok()
        .is_some_and(|count| {
            profiles[..count].iter().any(|profile| {
                profile.iccid_string().as_deref() == Ok(iccid)
                    && profile.state == ProfileState::Enabled
            })
        });
    let mut channel = card.into_inner().into_inner();
    let _ = channel.close();
    drop(channel);
    confirmed
}

fn classify_module_switch_error<E: std::fmt::Display>(
    error: &proletarius_euicc::Error<E>,
) -> ModuleSwitchFailure {
    let diagnostic = "EnableProfile failed and readback did not confirm the target";
    match error {
        proletarius_euicc::Error::ProfileOperation {
            result: ProfileOperationResult::CatBusy,
            ..
        } => ModuleSwitchFailure {
            status: StatusCode::CONFLICT,
            code: "profile_switch_card_busy",
            message: "卡片仍在处理上一项操作，请等待完成后刷新状态",
            diagnostic,
        },
        proletarius_euicc::Error::ProfileOperation {
            result:
                ProfileOperationResult::DisallowedByPolicy
                | ProfileOperationResult::DisallowedByEnterpriseRule
                | ProfileOperationResult::DisallowedForRpm
                | ProfileOperationResult::NoEsimPortAvailable,
            ..
        } => ModuleSwitchFailure {
            status: StatusCode::BAD_GATEWAY,
            code: "profile_switch_policy_denied",
            message: "卡片策略不允许启用此 Profile，请核对号卡服务商的限制",
            diagnostic,
        },
        proletarius_euicc::Error::ProfileOperation {
            result: ProfileOperationResult::IccidOrAidNotFound,
            ..
        } => ModuleSwitchFailure {
            status: StatusCode::NOT_FOUND,
            code: "profile_switch_target_missing",
            message: "卡片未找到目标 Profile，请刷新列表后重新选择",
            diagnostic,
        },
        proletarius_euicc::Error::ProfileOperation { .. } => ModuleSwitchFailure {
            status: StatusCode::BAD_GATEWAY,
            code: "profile_switch_card_rejected",
            message: "卡片拒绝启用 Profile；请刷新卡片状态并核对兼容性，不要连续重复切卡",
            diagnostic,
        },
        _ => ModuleSwitchFailure {
            status: StatusCode::BAD_GATEWAY,
            code: "profile_switch_failed",
            message: "切换 Profile 未完成；请刷新卡片状态确认当前启用的号卡，不要连续重复切卡",
            diagnostic,
        },
    }
}

fn rename_profile_on_transport(
    transport: &mut dyn AtCommandTransport<Error = usb_at::Failure>,
    iccid: &str,
    requested_aid: Option<&[u8]>,
    name: &str,
) -> Result<(), usb_at::Failure> {
    let encoded_iccid = encode_iccid(iccid).map_err(|_| rename_transport_failure())?;
    let candidates: Vec<&[u8]> = requested_aid
        .map(|aid| vec![aid])
        .unwrap_or_else(|| KNOWN_ISDR_APPLICATION_AIDS.to_vec());
    for aid in candidates {
        let channel = match ModuleEuiccChannel::open(transport, aid) {
            Ok(channel) => channel,
            Err(ChannelError::ModemRejected) if requested_aid.is_none() => continue,
            Err(_) => return Err(rename_transport_failure()),
        };
        let channel_number = channel.channel().ok_or_else(rename_transport_failure)?;
        let mut card = Euicc::new(ApduCardTransport::new(channel), channel_number, DEFAULT_MSS)
            .map_err(|_| rename_transport_failure())?;
        let operation =
            card.set_nickname(ProfileIdentifier::Iccid(&encoded_iccid), name.as_bytes());
        let mut channel = card.into_inner().into_inner();
        let close = channel.close();
        drop(channel);
        match operation {
            Ok(()) => return close.map_err(|_| rename_transport_failure()),
            Err(proletarius_euicc::Error::NicknameOperation {
                result: proletarius_euicc::NicknameResult::IccidNotFound,
            }) if requested_aid.is_none() => {
                close.map_err(|_| rename_transport_failure())?;
            }
            Err(_) => return Err(rename_transport_failure()),
        }
    }
    Err(rename_transport_failure())
}

fn rename_transport_failure() -> usb_at::Failure {
    usb_at::Failure::new(
        StatusCode::BAD_GATEWAY,
        "rename_profile_failed",
        "修改 Profile 名称失败，请稍后重试",
    )
}

struct CardSnapshot {
    eid: String,
    eid_bytes: Option<Vec<u8>>,
    eid_info: Value,
    profiles: Value,
}

/// The recovery evidence is deliberately separate from the deduplicated UI
/// projection. Two ISD-R AIDs may expose one EID while only one AID's profile
/// list completed; the other AID cannot inherit that proof.
pub(crate) struct OverviewRead {
    pub(crate) value: Value,
    pub(crate) complete_card_identities: Vec<(Vec<u8>, Vec<u8>)>,
}

pub(crate) fn read_overview(
    transport: &mut dyn AtCommandTransport<Error = usb_at::Failure>,
) -> Result<Value, usb_at::Failure> {
    read_overview_with_identities(transport).map(|read| read.value)
}

pub(crate) fn read_overview_with_identities(
    transport: &mut dyn AtCommandTransport<Error = usb_at::Failure>,
) -> Result<OverviewRead, usb_at::Failure> {
    let mut eid_infos = Vec::new();
    let mut profile_groups = Vec::new();
    let mut complete_card_identities = Vec::new();
    let mut seen_eids = HashSet::new();
    let mut successful_aids: Vec<&[u8]> = Vec::new();

    for aid in KNOWN_ISDR_APPLICATION_AIDS {
        if !successful_aids.is_empty()
            && !successful_aids.iter().any(|found| {
                reader_esim_download::is_estkme_application_aid(found)
                    && reader_esim_download::is_estkme_application_aid(aid)
            })
        {
            break;
        }

        let snapshot = match read_card(transport, aid) {
            Ok(snapshot) => snapshot,
            Err(ChannelError::ModemRejected) => continue,
            Err(ChannelError::Transport(_)) => return Err(overview_failure()),
            Err(_) => return Err(overview_failure()),
        };
        successful_aids.push(aid);
        if snapshot.profiles["read_state"] == "ready"
            && let Some(eid_bytes) = snapshot.eid_bytes.as_ref()
        {
            complete_card_identities.push((eid_bytes.clone(), aid.to_vec()));
        }
        if seen_eids.insert(snapshot.eid.clone()) {
            eid_infos.push(snapshot.eid_info);
            profile_groups.push(snapshot.profiles);
        }
    }

    if eid_infos.is_empty() {
        return Err(usb_at::Failure::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "euicc_not_accessible",
            "模块当前未能访问 eUICC（可能是实体 SIM，也可能是模块 APDU 通道暂不可用）。请确认已启用 eSIM Profile，关闭 VoWiFi 后刷新。",
        ));
    }

    let (sku_name, serial_number, firmware) =
        read_product_info(transport, &successful_aids).map_err(|_| overview_failure())?;
    let mut chip_info = json!({"eids":eid_infos});
    if let Some(value) = sku_name {
        chip_info["sku_name"] = json!(value);
    }
    if let Some(value) = serial_number {
        chip_info["serial_number"] = json!(value);
    }
    if let Some(value) = firmware {
        chip_info["firmware"] = json!(value);
    }

    let updated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "0001-01-01T00:00:00Z".into());
    Ok(OverviewRead {
        value: json!({
            "chip_info": chip_info,
            "profiles": profile_groups,
            "updated_at": updated_at
        }),
        complete_card_identities,
    })
}

fn read_card<T: AtCommandTransport + ?Sized>(
    transport: &mut T,
    aid: &[u8],
) -> Result<CardSnapshot, ChannelError<T::Error>> {
    let channel = ModuleEuiccChannel::open(transport, aid)?;
    let channel_number = channel.channel().ok_or(ChannelError::NoChannel)?;
    let mut card = Euicc::new(ApduCardTransport::new(channel), channel_number, DEFAULT_MSS)
        .map_err(|_| ChannelError::MalformedResponse)?;

    let card_result = (|| {
        let mut eid_bytes = [0_u8; EID_LENGTH_BYTES];
        let eid_length = card
            .get_eid(&mut eid_bytes)
            .map_err(|_| ChannelError::MalformedResponse)?;
        let eid = reader_esim_download::encode_hex_lower(&eid_bytes[..eid_length]);
        let proof_eid = (eid_length == EID_LENGTH_BYTES
            && eid_bytes
                .iter()
                .all(|byte| byte >> 4 <= 9 && byte & 0x0f <= 9))
        .then(|| eid_bytes.to_vec());

        let euicc_info = card.get_euicc_info_preferred().ok();
        let nvram_bytes = euicc_info
            .as_ref()
            .and_then(|info| info.free_nvram_bytes)
            .unwrap_or_default();
        let mut eid_info = json!({
            "aid": reader_esim_download::encode_hex_upper(aid),
            "eid": eid,
            "spec_guess": "sgp22_compatible",
            "spec_confidence": "inferred",
            "free_nvram_bytes": nvram_bytes.min(i32::MAX as u64),
            "free_nvram": reader_esim_download::format_nvram(nvram_bytes),
        });
        if let Some(info) = euicc_info.as_ref() {
            if let Some(firmware) = info.firmware_string() {
                eid_info["firmware"] = json!(firmware);
            }
            eid_info["info_source"] = json!(format!("euicc_info{}", info.version));
            eid_info["info_version"] = json!(info.version.to_string());
            let sas = info.sas_accreditation_number_str().unwrap_or_default();
            if !sas.is_empty() {
                eid_info["sas_accreditation_number"] = json!(sas);
            }
            if let Some(manufacturer) = reader_esim_download::lookup_euicc_manufacturer(&eid, sas) {
                eid_info["manufacturer"] = json!(manufacturer);
            }
            let certificates =
                reader_esim_download::lookup_certificate_issuers(&info.certificate_key_ids);
            if !certificates.is_empty() {
                eid_info["certificates"] = json!(certificates);
            }
        }
        if let Ok(addresses) = card.get_configured_addresses() {
            if let Some(address) = addresses
                .default_smdp_address_str()
                .filter(|address| !address.is_empty())
            {
                eid_info["default_smdp_address"] = json!(address);
            }
            if let Some(address) = addresses
                .root_smds_address_str()
                .filter(|address| !address.is_empty())
            {
                eid_info["root_ds_address"] = json!(address);
            }
        }

        let mut profiles = vec![ProfileInfo::default(); MAX_PROFILE_COUNT];
        let profile_group = match card.list_profiles_into(&mut profiles) {
            Ok(count) => json!({
                "eid": eid,
                "aid_hex": reader_esim_download::encode_hex_upper(aid),
                "read_state": "ready",
                "profiles": profiles[..count]
                    .iter()
                    .map(reader_esim_download::overview_profile)
                    .collect::<Vec<_>>(),
            }),
            Err(_) => json!({
                "eid": eid,
                "aid_hex": reader_esim_download::encode_hex_upper(aid),
                "read_state": "error",
                "read_error": PROFILE_READ_ERROR,
                "profiles": [],
            }),
        };
        Ok(CardSnapshot {
            eid,
            eid_bytes: proof_eid,
            eid_info,
            profiles: profile_group,
        })
    })();

    let mut channel = card.into_inner().into_inner();
    let close_result = channel.close();
    drop(channel);
    match (card_result, close_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (_, Err(cleanup_error)) => Err(cleanup_error),
        (Err(read_error), Ok(())) => Err(read_error),
    }
}

fn read_product_info<T: AtCommandTransport + ?Sized>(
    transport: &mut T,
    profile_aids: &[&[u8]],
) -> Result<(Option<String>, Option<String>, Option<String>), ChannelError<T::Error>> {
    if !profile_aids
        .iter()
        .any(|aid| reader_esim_download::is_estkme_application_aid(aid))
    {
        return Ok((None, None, None));
    }
    let channel = match ModuleEuiccChannel::open(
        transport,
        proletarius_euicc::ESTKME_PRODUCT_APPLICATION_AID,
    ) {
        Ok(channel) => channel,
        Err(ChannelError::ModemRejected) => return Ok((None, None, None)),
        Err(error) => return Err(error),
    };
    let channel_number = channel.channel().ok_or(ChannelError::NoChannel)?;
    let mut card = Euicc::new(ApduCardTransport::new(channel), channel_number, DEFAULT_MSS)
        .map_err(|_| ChannelError::MalformedResponse)?;
    let sku_name = reader_esim_download::read_product_field(&mut card, ProductField::SkuName);
    let serial_number =
        reader_esim_download::read_product_field(&mut card, ProductField::SerialNumber);
    let bootloader = reader_esim_download::read_product_field(&mut card, ProductField::Bootloader);
    let product_firmware =
        reader_esim_download::read_product_field(&mut card, ProductField::Firmware);
    let firmware = match (bootloader, product_firmware) {
        (Some(bootloader), Some(firmware)) => Some(format!("{bootloader}-{firmware}")),
        (None, firmware) => firmware,
        (Some(_), None) => None,
    };
    let mut channel = card.into_inner().into_inner();
    let close_result = channel.close();
    drop(channel);
    close_result?;
    Ok((sku_name, serial_number, firmware))
}

fn overview_failure() -> usb_at::Failure {
    usb_at::Failure::new(
        StatusCode::BAD_GATEWAY,
        "esim_overview_failed",
        "读取 eSIM 卡片信息失败，请重新连接模块后重试",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::Duration;

    use hyper::StatusCode;
    use proletarius_ccid::bertlv;
    use proletarius_euicc::encode_iccid;

    use crate::reader_esim_download;

    use super::{
        KNOWN_ISDR_APPLICATION_AIDS, decode_aid, delete_profile_on_transport, parse_module_iccid,
        read_overview, read_overview_with_identities, read_product_info,
        rename_profile_on_transport, switch_profile_on_transport_with_wait,
    };
    use crate::module_esim_channel::{AtCommandTransport, Error as ChannelError};
    use crate::usb_at;

    struct FakeAt {
        replies: VecDeque<Result<String, usb_at::Failure>>,
        commands: Vec<String>,
    }

    impl AtCommandTransport for FakeAt {
        type Error = usb_at::Failure;

        fn command(
            &mut self,
            command: &str,
            _timeout: Duration,
            _max_response_bytes: usize,
        ) -> Result<String, Self::Error> {
            self.commands.push(command.to_owned());
            self.replies.pop_front().expect("scripted AT reply")
        }
    }

    fn cgla_reply(data: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut encoded = String::with_capacity(data.len() * 2);
        for byte in data {
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        format!("+CGLA: {},\"{encoded}\"\r\nOK\r\n", encoded.len())
    }

    fn profile_list_reply(iccid: &str, state: u8) -> String {
        fn tlv(tag: &[u8], value: &[u8]) -> Vec<u8> {
            assert!(value.len() < 128);
            let mut encoded = Vec::with_capacity(tag.len() + value.len() + 2);
            encoded.extend_from_slice(tag);
            encoded.push(value.len() as u8);
            encoded.extend_from_slice(value);
            encoded
        }
        let encoded_iccid = encode_iccid(iccid).unwrap();
        let mut fields = tlv(&[0x5a], &encoded_iccid);
        fields.extend_from_slice(&tlv(&[0x9f, 0x70], &[state]));
        let profile = tlv(&[0xe3], &fields);
        let profile_list = tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &profile));
        let mut response = profile_list;
        response.extend_from_slice(&[0x90, 0x00]);
        cgla_reply(&response)
    }

    fn eid_reply(eid: &[u8; super::EID_LENGTH_BYTES]) -> String {
        let mut eid_tlv = Vec::new();
        bertlv::encode(&[0x5a], eid, &mut eid_tlv).expect("encode EID");
        let mut get_eid = Vec::new();
        bertlv::encode_constructed(&[0xbf, 0x3e], &eid_tlv, &mut get_eid)
            .expect("encode GetEID response");
        get_eid.extend_from_slice(&[0x90, 0x00]);
        cgla_reply(&get_eid)
    }

    #[test]
    fn module_switch_enables_disabled_profile_once_and_closes_channel() {
        let iccid = "8985200014631959072";
        let encoded_iccid = encode_iccid(iccid).unwrap();
        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(profile_list_reply(iccid, 0)),
                Ok(eid_reply(&[0x12; super::EID_LENGTH_BYTES])),
                Ok(cgla_reply(&[
                    0xbf, 0x31, 0x03, 0x80, 0x01, 0x00, 0x90, 0x00,
                ])),
                Ok("OK\r\n".into()),
            ]),
            commands: Vec::new(),
        };

        let result = switch_profile_on_transport_with_wait(
            &mut transport,
            iccid,
            Some(&KNOWN_ISDR_APPLICATION_AIDS[0]),
            |eid, aid| {
                assert_eq!(eid, &[0x12; super::EID_LENGTH_BYTES]);
                assert_eq!(aid, KNOWN_ISDR_APPLICATION_AIDS[0]);
                Ok(())
            },
            |_| {},
        )
        .expect("profile switch is accepted");

        assert!(result.warning.is_none());
        assert_eq!(transport.commands.len(), 5);
        assert!(transport.commands[0].starts_with("AT+CCHO=\""));
        assert!(transport.commands[1].starts_with("AT+CGLA=1,"));
        assert!(transport.commands[2].starts_with("AT+CGLA=1,"));
        assert!(
            transport.commands[3].contains(
                &encoded_iccid
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<String>()
            )
        );
        assert_eq!(transport.commands[4], "AT+CCHC=1");
    }

    #[test]
    fn module_switch_readback_accepts_uncertain_enable_without_replaying_it() {
        let iccid = "8985200014631959072";
        let encoded_iccid = encode_iccid(iccid).unwrap();
        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(profile_list_reply(iccid, 0)),
                Ok(eid_reply(&[0x12; super::EID_LENGTH_BYTES])),
                Err(usb_at::Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "test_uncertain_apdu",
                    "simulated lost APDU response",
                )),
                Ok("OK\r\n".into()),
                Ok("+CCHO: 2\r\nOK\r\n".into()),
                Ok(profile_list_reply(iccid, 1)),
                Ok("OK\r\n".into()),
            ]),
            commands: Vec::new(),
        };

        let result = switch_profile_on_transport_with_wait(
            &mut transport,
            iccid,
            Some(&KNOWN_ISDR_APPLICATION_AIDS[0]),
            |_, _| Ok(()),
            |_| {},
        )
        .expect("readback confirms the enable operation");

        assert_eq!(
            result.warning.as_ref().map(|(_, code)| code.as_str()),
            Some("switch_profile_response_uncertain")
        );
        assert_eq!(transport.commands.len(), 8);
        let encoded_iccid = encoded_iccid
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>();
        assert_eq!(
            transport
                .commands
                .iter()
                .filter(|command| command.contains(&encoded_iccid))
                .count(),
            1,
            "uncertain EnableProfile must never be replayed"
        );
        assert_eq!(transport.commands[4], "AT+CCHC=1");
        assert!(transport.commands[5].starts_with("AT+CCHO=\""));
        assert_eq!(transport.commands[7], "AT+CCHC=2");
    }

    #[test]
    fn module_switch_does_not_probe_other_aids_after_6982_profile_read() {
        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(cgla_reply(&[0x69, 0x82])),
                Ok("OK\r\n".into()),
            ]),
            commands: Vec::new(),
        };

        let error = switch_profile_on_transport_with_wait(
            &mut transport,
            "8985200014631959072",
            None,
            |_, _| panic!("proof must not arm after failed profile read"),
            |_| {},
        )
        .expect_err("authenticated profile read failure must fail closed");
        assert_eq!(error.code, "profile_switch_failed");
        assert_eq!(transport.commands.len(), 3);
        assert_eq!(
            transport.commands[0],
            format!(
                "AT+CCHO=\"{}\"",
                super::reader_esim_download::encode_hex_upper(KNOWN_ISDR_APPLICATION_AIDS[0])
            )
        );
        assert_eq!(transport.commands[2], "AT+CCHC=1");
    }

    #[test]
    fn module_switch_proof_write_failure_never_submits_enable_profile() {
        let iccid = "8985200014631959072";
        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(profile_list_reply(iccid, 0)),
                Ok(eid_reply(&[0x12; super::EID_LENGTH_BYTES])),
                Ok("OK\r\n".into()),
            ]),
            commands: Vec::new(),
        };
        let mut proof_attempts = 0;
        let error = switch_profile_on_transport_with_wait(
            &mut transport,
            iccid,
            Some(&KNOWN_ISDR_APPLICATION_AIDS[0]),
            |eid, aid| {
                proof_attempts += 1;
                assert_eq!(eid, &[0x12; super::EID_LENGTH_BYTES]);
                assert_eq!(aid, KNOWN_ISDR_APPLICATION_AIDS[0]);
                Err("fixture_journal_failure")
            },
            |_| {},
        )
        .expect_err("uncommitted proof must stop before EnableProfile");
        assert_eq!(error.code, "esim_operation_proof_unavailable");
        assert_eq!(proof_attempts, 1);
        assert_eq!(transport.commands.len(), 4);
        assert_eq!(transport.commands[3], "AT+CCHC=1");
    }

    #[test]
    fn module_switch_invalid_eid_never_arms_proof_or_submits_enable_profile() {
        let iccid = "8985200014631959072";
        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(profile_list_reply(iccid, 0)),
                Ok(eid_reply(&[0xfa; super::EID_LENGTH_BYTES])),
                Ok("OK\r\n".into()),
            ]),
            commands: Vec::new(),
        };
        let error = switch_profile_on_transport_with_wait(
            &mut transport,
            iccid,
            Some(&KNOWN_ISDR_APPLICATION_AIDS[0]),
            |_, _| panic!("invalid EID cannot reach proof writer"),
            |_| {},
        )
        .expect_err("invalid EID must stop before EnableProfile");
        assert_eq!(error.code, "profile_switch_identity_unconfirmed");
        assert_eq!(transport.commands.len(), 4);
        assert_eq!(transport.commands[3], "AT+CCHC=1");
    }

    #[test]
    fn module_iccid_parser_requires_one_well_formed_identity_line() {
        assert_eq!(
            parse_module_iccid("+QCCID: \"8985200014631959072F\"\r\nOK\r\n"),
            Some("8985200014631959072F".into())
        );
        assert_eq!(parse_module_iccid("+QCCID: 123\r\n+QCCID: 456\r\n"), None);
        assert_eq!(parse_module_iccid("+QCCID: 12x\r\n"), None);
    }

    #[test]
    fn failed_card_read_closes_the_logical_channel_before_returning() {
        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok("+CGLA: 4,\"6A82\"\r\nOK\r\n".into()),
                Ok("OK\r\n".into()),
            ]),
            commands: Vec::new(),
        };

        let failure = read_overview(&mut transport).expect_err("card response is rejected");
        assert_eq!(failure.public_code(), "esim_overview_failed");
        assert_eq!(transport.commands.len(), 3);
        assert!(transport.commands[0].starts_with("AT+CCHO=\""));
        assert!(transport.commands[1].starts_with("AT+CGLA=1,"));
        assert_eq!(transport.commands[2], "AT+CCHC=1");
    }

    #[test]
    fn all_rejected_isdr_aids_are_reported_as_euicc_unavailable() {
        let mut transport = FakeAt {
            replies: VecDeque::from(
                proletarius_euicc::KNOWN_ISDR_APPLICATION_AIDS
                    .iter()
                    .map(|_| Ok("ERROR\r\n".into()))
                    .collect::<Vec<_>>(),
            ),
            commands: Vec::new(),
        };

        let failure = read_overview(&mut transport).expect_err("no eUICC AID is available");
        assert_eq!(failure.public_status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(failure.public_code(), "euicc_not_accessible");
        assert_eq!(
            transport.commands.len(),
            proletarius_euicc::KNOWN_ISDR_APPLICATION_AIDS.len()
        );
        assert!(
            transport
                .commands
                .iter()
                .all(|command| command.starts_with("AT+CCHO=\""))
        );
    }

    #[test]
    fn successful_eid_read_preserves_partial_profile_failure_as_group_state() {
        let eid = [0x12_u8; proletarius_euicc::EID_LENGTH_BYTES];
        let mut eid_tlv = Vec::new();
        bertlv::encode(&[0x5a], &eid, &mut eid_tlv).expect("encode EID");
        let mut get_eid = Vec::new();
        bertlv::encode_constructed(&[0xbf, 0x3e], &eid_tlv, &mut get_eid)
            .expect("encode GetEID response");
        get_eid.extend_from_slice(&[0x90, 0x00]);

        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(cgla_reply(&get_eid)),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok("OK\r\n".into()),
                Ok("ERROR\r\n".into()),
                Ok("ERROR\r\n".into()),
            ]),
            commands: Vec::new(),
        };

        let overview = read_overview(&mut transport).expect("valid EID yields partial overview");
        assert_eq!(
            overview["chip_info"]["eids"][0]["eid"],
            "12121212121212121212121212121212"
        );
        assert_eq!(overview["profiles"][0]["read_state"], "error");
        assert_eq!(overview["profiles"][0]["profiles"], serde_json::json!([]));
        assert_eq!(transport.commands.len(), 9);
        assert_eq!(transport.commands[6], "AT+CCHC=1");
        assert!(transport.commands[7].starts_with("AT+CCHO=\""));
        assert!(transport.commands[8].starts_with("AT+CCHO=\""));
    }

    #[test]
    fn successful_profile_list_keeps_legacy_overview_fields() {
        let eid = [0x12_u8; proletarius_euicc::EID_LENGTH_BYTES];
        let mut eid_tlv = Vec::new();
        bertlv::encode(&[0x5a], &eid, &mut eid_tlv).expect("encode EID");
        let mut get_eid = Vec::new();
        bertlv::encode_constructed(&[0xbf, 0x3e], &eid_tlv, &mut get_eid)
            .expect("encode GetEID response");
        get_eid.extend_from_slice(&[0x90, 0x00]);

        let iccid = proletarius_euicc::encode_iccid("8901000000000000000").expect("encode ICCID");
        let mut profile_fields = Vec::new();
        bertlv::encode(&[0x5a], &iccid, &mut profile_fields).expect("encode ICCID TLV");
        bertlv::encode(&[0x9f, 0x70], &[0x01], &mut profile_fields).expect("encode state TLV");
        bertlv::encode(&[0x90], b"My profile", &mut profile_fields).expect("encode nickname TLV");
        bertlv::encode(&[0x91], b"SIMworld", &mut profile_fields).expect("encode provider TLV");
        bertlv::encode(&[0x95], &[0x02], &mut profile_fields).expect("encode class TLV");
        let mut profile = Vec::new();
        bertlv::encode_constructed(&[0xe3], &profile_fields, &mut profile).expect("encode profile");
        let mut profiles = Vec::new();
        bertlv::encode_constructed(&[0xa0], &profile, &mut profiles).expect("encode profile list");
        let mut profile_list = Vec::new();
        bertlv::encode_constructed(&[0xbf, 0x2d], &profiles, &mut profile_list)
            .expect("encode GetProfilesInfo response");
        profile_list.extend_from_slice(&[0x90, 0x00]);

        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(cgla_reply(&get_eid)),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok(cgla_reply(&profile_list)),
                Ok("OK\r\n".into()),
                Ok("ERROR\r\n".into()),
                Ok("ERROR\r\n".into()),
            ]),
            commands: Vec::new(),
        };

        let overview = read_overview(&mut transport).expect("valid profile list");
        let item = &overview["profiles"][0]["profiles"][0];
        assert_eq!(overview["profiles"][0]["read_state"], "ready");
        assert_eq!(item["iccid"], "8901000000000000000");
        assert_eq!(item["name"], "My profile");
        assert_eq!(item["service_provider_name"], "SIMworld");
        assert_eq!(item["state"], 1);
        assert_eq!(item["state_text"], "已启用");
        assert_eq!(item["class_text"], "operational");
    }

    #[test]
    fn duplicate_eid_aids_require_their_own_completed_profile_reads_for_private_proof() {
        let eid = [0x12_u8; proletarius_euicc::EID_LENGTH_BYTES];
        let mut eid_tlv = Vec::new();
        bertlv::encode(&[0x5a], &eid, &mut eid_tlv).unwrap();
        let mut get_eid = Vec::new();
        bertlv::encode_constructed(&[0xbf, 0x3e], &eid_tlv, &mut get_eid).unwrap();
        get_eid.extend_from_slice(&[0x90, 0x00]);

        let iccid = encode_iccid("8901000000000000000").unwrap();
        let mut profile_fields = Vec::new();
        bertlv::encode(&[0x5a], &iccid, &mut profile_fields).unwrap();
        bertlv::encode(&[0x9f, 0x70], &[0x01], &mut profile_fields).unwrap();
        let mut profile = Vec::new();
        bertlv::encode_constructed(&[0xe3], &profile_fields, &mut profile).unwrap();
        let mut profiles = Vec::new();
        bertlv::encode_constructed(&[0xa0], &profile, &mut profiles).unwrap();
        let mut profile_list = Vec::new();
        bertlv::encode_constructed(&[0xbf, 0x2d], &profiles, &mut profile_list).unwrap();
        profile_list.extend_from_slice(&[0x90, 0x00]);

        let channel_replies = |ready: bool| {
            Vec::<Result<String, usb_at::Failure>>::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(cgla_reply(&get_eid)),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok(cgla_reply(&[0x6a, 0x81])),
                Ok(cgla_reply(if ready {
                    &profile_list[..]
                } else {
                    &[0x6a, 0x81]
                })),
                Ok("OK\r\n".into()),
            ])
        };
        let mut complete_replies = channel_replies(true);
        complete_replies.extend(channel_replies(true));
        complete_replies.push(Ok("ERROR\r\n".into())); // optional product applet
        let mut complete = FakeAt {
            replies: VecDeque::from(complete_replies),
            commands: Vec::new(),
        };
        let result = read_overview_with_identities(&mut complete).unwrap();
        assert_eq!(
            result.value["chip_info"]["eids"].as_array().unwrap().len(),
            1
        );
        assert_eq!(result.value["profiles"].as_array().unwrap().len(), 1);
        assert_eq!(result.complete_card_identities.len(), 2);
        assert_eq!(result.complete_card_identities[0].0, eid);
        assert_eq!(
            result.complete_card_identities[0].1,
            KNOWN_ISDR_APPLICATION_AIDS[0]
        );
        assert_eq!(
            result.complete_card_identities[1].1,
            KNOWN_ISDR_APPLICATION_AIDS[1]
        );

        let mut partial_replies = channel_replies(true);
        partial_replies.extend(channel_replies(false));
        partial_replies.push(Ok("ERROR\r\n".into()));
        let mut partial = FakeAt {
            replies: VecDeque::from(partial_replies),
            commands: Vec::new(),
        };
        let result = read_overview_with_identities(&mut partial).unwrap();
        assert_eq!(result.value["profiles"].as_array().unwrap().len(), 1);
        assert_eq!(result.complete_card_identities.len(), 1);
        assert_eq!(
            result.complete_card_identities[0].1,
            KNOWN_ISDR_APPLICATION_AIDS[0]
        );
    }

    #[test]
    fn optional_product_metadata_does_not_hide_channel_cleanup_failure() {
        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(cgla_reply(&[0x6a, 0x82])),
                Ok(cgla_reply(&[0x6a, 0x82])),
                Ok(cgla_reply(&[0x6a, 0x82])),
                Ok(cgla_reply(&[0x6a, 0x82])),
                Ok("ERROR\r\n".into()),
            ]),
            commands: Vec::new(),
        };

        let result = read_product_info(
            &mut transport,
            &[proletarius_euicc::ESTKME_SE0_APPLICATION_AID],
        );
        assert!(matches!(result, Err(ChannelError::ChannelCleanupFailed)));
        assert_eq!(
            transport.commands.last().map(String::as_str),
            Some("AT+CCHC=1")
        );
    }

    #[test]
    fn rename_uses_one_open_apdu_close_transaction_and_preserves_success_shape() {
        let mut response = Vec::new();
        bertlv::encode_constructed(&[0xbf, 0x29], &[0x80, 0x01, 0x00], &mut response)
            .expect("encode nickname result");
        response.extend_from_slice(&[0x90, 0x00]);
        let mut transport = FakeAt {
            replies: VecDeque::from([
                Ok("+CCHO: 1\r\nOK\r\n".into()),
                Ok(cgla_reply(&response)),
                Ok("OK\r\n".into()),
            ]),
            commands: Vec::new(),
        };
        let aid = decode_aid("A0000005591010FFFFFFFF8900000100")
            .expect("valid AID")
            .expect("provided AID");

        rename_profile_on_transport(
            &mut transport,
            "8901000000000000000",
            Some(&aid),
            "旅行号码",
        )
        .expect("nickname update accepted");

        assert_eq!(transport.commands.len(), 3);
        assert_eq!(
            transport.commands[0],
            "AT+CCHO=\"A0000005591010FFFFFFFF8900000100\""
        );
        assert!(transport.commands[1].starts_with("AT+CGLA=1,"));
        assert_eq!(transport.commands[2], "AT+CCHC=1");
    }

    #[test]
    fn delete_reuses_profile_notification_flow_inside_one_module_at_channel() {
        let iccid = "8901000000000000000";
        let encoded_iccid = proletarius_euicc::encode_iccid(iccid).unwrap();
        let mut profile_fields = Vec::new();
        bertlv::encode(&[0x5a], &encoded_iccid, &mut profile_fields).unwrap();
        let mut profile = Vec::new();
        bertlv::encode_constructed(&[0xe3], &profile_fields, &mut profile).unwrap();
        let mut profile_list_items = Vec::new();
        bertlv::encode_constructed(&[0xa0], &profile, &mut profile_list_items).unwrap();
        let mut profile_list = Vec::new();
        bertlv::encode_constructed(&[0xbf, 0x2d], &profile_list_items, &mut profile_list).unwrap();
        profile_list.extend_from_slice(&[0x90, 0x00]);

        let mut responses = vec![
            Ok("+CCHO: 1\r\nOK\r\n".into()),
            Ok(cgla_reply(&[0xbf, 0x28, 0x02, 0xa0, 0x00, 0x90, 0x00])), // Notification cursor
            Ok(cgla_reply(&profile_list)),
            Ok(cgla_reply(&[0x6a, 0x81])), // EUICCInfo2 unsupported
            Ok(cgla_reply(&[0x6a, 0x81])), // EUICCInfo1 unsupported
            Ok(cgla_reply(&[
                0xbf, 0x33, 0x03, 0x80, 0x01, 0x00, 0x90, 0x00,
            ])), // DELETE PROFILE accepted
            Ok(cgla_reply(&[0xbf, 0x2d, 0x02, 0xa0, 0x00, 0x90, 0x00])), // Empty profile list
            Ok(cgla_reply(&[0xbf, 0x28, 0x02, 0xa0, 0x00, 0x90, 0x00])),
            Ok(cgla_reply(&[0xbf, 0x28, 0x02, 0xa0, 0x00, 0x90, 0x00])),
            Ok(cgla_reply(&[0xbf, 0x28, 0x02, 0xa0, 0x00, 0x90, 0x00])),
        ];
        for _ in 0..6 {
            responses.push(Ok(cgla_reply(&[0x6a, 0x81])));
        }
        responses.push(Ok("OK\r\n".into())); // CCHC
        let mut transport = FakeAt {
            replies: VecDeque::from(responses),
            commands: Vec::new(),
        };

        let result = delete_profile_on_transport(
            &mut transport,
            iccid,
            Some(&KNOWN_ISDR_APPLICATION_AIDS[0]),
        )
        .unwrap()
        .expect("profile was deleted");

        assert_eq!(
            result.warning_code.as_deref(),
            Some("delete_notification_not_observed")
        );
        assert_eq!(
            transport.commands.first().unwrap(),
            &format!(
                "AT+CCHO=\"{}\"",
                reader_esim_download::encode_hex_upper(KNOWN_ISDR_APPLICATION_AIDS[0])
            )
        );
        assert_eq!(transport.commands.last().unwrap(), "AT+CCHC=1");
        assert_eq!(
            transport
                .commands
                .iter()
                .filter(|command| command.contains("BF33"))
                .count(),
            1
        );
    }

    #[test]
    fn rename_aid_parsing_fails_closed_on_bad_inputs() {
        assert_eq!(decode_aid(""), Some(None));
        assert_eq!(decode_aid("A000"), Some(Some(vec![0xa0, 0x00])));
        assert_eq!(decode_aid("A00"), None);
        assert_eq!(decode_aid("GG00"), None);
    }
}
