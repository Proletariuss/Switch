//! Public startup facts are typed, bounded, and independent of private error
//! strings. Never serialize a SIM/AKA error, SIP packet, or transport Debug.
use super::*;
use proletarius_ike::eap_aka::EapAkaError;
use proletarius_ims::{SipError, registration_io::RegisterWaitError};
use proletarius_vowifi::{
    EapAkaExchangeError, EapHandshakeError, IkeAuthError, Ikev2EapError, SaInitError,
    TunnelConfigError,
    registration_coordinator::{RegistrationError, RegistrationStage},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Diagnostic {
    pub(crate) stage: &'static str,
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    sip_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    eap_subtype: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    io_operation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    io_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os_error: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    esp_sent: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    esp_received: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    esp_rejected: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip_accepted: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_exit: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ike_tunnel_exit: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_io_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_ended_before_cleanup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ike_received_after_child: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keepalive_received_after_child: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invalid_datagram_after_child: Option<u64>,
}
fn diagnostic(stage: &'static str, code: &'static str, message: &'static str) -> Diagnostic {
    Diagnostic {
        stage,
        code,
        message,
        sip_status: None,
        eap_subtype: None,
        io_operation: None,
        io_kind: None,
        os_error: None,
        esp_sent: None,
        esp_received: None,
        esp_rejected: None,
        ip_accepted: None,
        network_exit: None,
        ike_tunnel_exit: None,
        network_io_kind: None,
        network_ended_before_cleanup: None,
        ike_received_after_child: None,
        keepalive_received_after_child: None,
        invalid_datagram_after_child: None,
    }
}
pub(super) fn startup(error: &NativeRegisteredStartupFailure) -> Diagnostic {
    let mut value = startup_base(error);
    if let NativeRegisteredStartupFailure::Registration {
        network,
        network_ended_before_cleanup,
        traffic,
        ..
    } = error
    {
        value.esp_sent = Some(traffic.sent_esp);
        value.esp_received = Some(traffic.received_esp);
        value.esp_rejected = Some(traffic.rejected_esp);
        value.ip_accepted = Some(traffic.accepted_ip);
        value.ike_received_after_child = Some(traffic.received_ike);
        value.keepalive_received_after_child = Some(traffic.received_keepalive);
        value.invalid_datagram_after_child = Some(traffic.invalid_datagram);
        value.network_ended_before_cleanup = Some(*network_ended_before_cleanup);
        apply_network_exit(&mut value, network, *network_ended_before_cleanup);
    }
    value
}

/// Preserve a bounded reason when the packet owner ended before IMS startup.
/// The join result is already owned by the failed startup; this projection
/// contains no endpoint, packet, key, SIM or SIP content.
fn apply_network_exit(
    value: &mut Diagnostic,
    result: &Result<
        Result<(), NativeNetworkError<std::convert::Infallible>>,
        tokio::task::JoinError,
    >,
    ended_before_cleanup: bool,
) {
    use proletarius_vowifi::netstack_runtime::NetworkRunError;

    let error = match result {
        Err(error) => {
            value.network_exit = Some(if error.is_cancelled() {
                "task_cancelled"
            } else if error.is_panic() {
                "task_panicked"
            } else {
                "task_join_failed"
            });
            return;
        }
        Ok(Ok(())) => {
            if ended_before_cleanup {
                value.network_exit = Some("ended_cleanly");
            }
            return;
        }
        Ok(Err(error)) => error,
    };
    match error {
        NativeNetworkError::Configuration(_) => value.network_exit = Some("configuration"),
        NativeNetworkError::Tunnel(error) => {
            value.network_exit = Some("ike_tunnel");
            value.ike_tunnel_exit = Some(ike_tunnel_exit(error));
        }
        NativeNetworkError::Network(error) => {
            value.network_exit = Some(match error {
                NetworkRunError::TunnelClosed => "tunnel_closed",
                NetworkRunError::SendTimeout => "send_timeout",
                NetworkRunError::Device(_) => "device",
                NetworkRunError::Application(never) => match *never {},
                NetworkRunError::SocketCapacity => "socket_capacity",
                NetworkRunError::PacketTransform(_) => "packet_transform",
            });
            if let NetworkRunError::PacketTransform(error) = error {
                value.network_io_kind = Some(io_kind(error.kind()));
            }
        }
    }
}

fn ike_tunnel_exit(error: &IkeAuthError) -> &'static str {
    use proletarius_vowifi::ChildDataError;

    match error {
        IkeAuthError::DataPlane(error) => match error {
            ChildDataError::PeerDeleted => "peer_deleted",
            ChildDataError::InvalidControlPacket => "invalid_control_packet",
            ChildDataError::EventConsumerUnavailable => "event_consumer_unavailable",
            ChildDataError::SendTimeout => "send_timeout",
            ChildDataError::UnsupportedAlgorithm => "unsupported_algorithm",
            ChildDataError::MissingConfiguration => "missing_configuration",
            ChildDataError::PolicyRejected(reason) => match reason {
                proletarius_vowifi::PolicyRejectReason::MalformedPacket => {
                    "policy_malformed_packet"
                }
                proletarius_vowifi::PolicyRejectReason::UnsupportedIpv6Extension => {
                    "policy_ipv6_extension"
                }
                proletarius_vowifi::PolicyRejectReason::UnassignedLocalAddress => {
                    "policy_unassigned_local"
                }
                proletarius_vowifi::PolicyRejectReason::SourceSelector => "policy_source_selector",
                proletarius_vowifi::PolicyRejectReason::DestinationSelector => {
                    "policy_destination_selector"
                }
            },
            ChildDataError::Esp(_) => "esp",
            ChildDataError::Transport(_) => "datagram_transport",
        },
        IkeAuthError::Configuration(_) => "configuration",
        IkeAuthError::TrafficSelectors(_) => "traffic_selectors",
        IkeAuthError::PeerNotify(_) => "peer_notify",
        IkeAuthError::MalformedNotify => "malformed_notify",
        IkeAuthError::ChildSa(_) => "child_sa",
        IkeAuthError::ResponderAuthenticationFailed => "responder_authentication_failed",
        IkeAuthError::KeyDerivation(_) => "key_derivation",
        IkeAuthError::InvalidAttempts => "invalid_attempts",
        IkeAuthError::PlannedReauthentication => "planned_reauthentication",
        IkeAuthError::Timeout => "timeout",
        IkeAuthError::UnexpectedPacket => "unexpected_packet",
        IkeAuthError::Ike(_) => "ike_protocol",
        IkeAuthError::Sk(_) => "encrypted_payload",
        IkeAuthError::Transport(_) => "datagram_transport",
    }
}

fn io_kind(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::TimedOut => "timed_out",
        std::io::ErrorKind::ConnectionRefused => "connection_refused",
        std::io::ErrorKind::ConnectionReset => "connection_reset",
        std::io::ErrorKind::ConnectionAborted => "connection_aborted",
        std::io::ErrorKind::NotConnected => "not_connected",
        std::io::ErrorKind::AddrNotAvailable => "address_not_available",
        std::io::ErrorKind::NetworkUnreachable => "network_unreachable",
        std::io::ErrorKind::HostUnreachable => "host_unreachable",
        std::io::ErrorKind::InvalidData => "invalid_data",
        std::io::ErrorKind::BrokenPipe => "broken_pipe",
        _ => "other",
    }
}
fn startup_base(error: &NativeRegisteredStartupFailure) -> Diagnostic {
    match error {
        NativeRegisteredStartupFailure::SessionMismatch => diagnostic(
            "admission",
            "session_identity_mismatch",
            "VoWiFi 会话与读卡器身份不一致，请重新连接",
        ),
        NativeRegisteredStartupFailure::ServiceGone { .. } => diagnostic(
            "finalize",
            "session_owner_unavailable",
            "VoWiFi 会话服务已退出，正在确认资源释放",
        ),
        NativeRegisteredStartupFailure::Tunnel(error) => tunnel(error),
        NativeRegisteredStartupFailure::Registration { error, .. } => match error {
            NativeImsRegistrationError::SessionMismatch => diagnostic(
                "ims_identity",
                "session_identity_mismatch",
                "IMS 身份与当前号卡会话不一致",
            ),
            NativeImsRegistrationError::TransportClosed => {
                diagnostic("ims_transport", "transport_closed", "IMS 注册连接已关闭")
            }
            NativeImsRegistrationError::Endpoint(_) => diagnostic(
                "ims_endpoint",
                "pcscf_unavailable",
                "IMS 注册端点不可用，请检查 P-CSCF 与隧道地址族配置",
            ),
            NativeImsRegistrationError::Entropy => diagnostic(
                "ims_configuration",
                "entropy_unavailable",
                "IMS 注册随机数生成失败",
            ),
            NativeImsRegistrationError::CancellationOwner(_) => diagnostic(
                "finalize",
                "cancellation_owner_mismatch",
                "IMS 注册取消归属校验失败，正在确认资源释放",
            ),
            NativeImsRegistrationError::Registration(failure) => {
                if let Some(profile) = failure.profile_error() {
                    return match profile {
                        NativeImsProfileError::UnsupportedSecurityMechanism
                        | NativeImsProfileError::NoNativeSecurityMechanism => diagnostic(
                            "ims_security",
                            "security_unsupported",
                            "IMS 安全机制不受支持，未建立安全注册",
                        ),
                        _ => diagnostic(
                            "ims_configuration",
                            "profile_invalid",
                            "IMS 运营商配置或注册模板不符合当前会话要求",
                        ),
                    };
                }
                registration(failure.registration().cause())
            }
        },
    }
}
fn tunnel(error: &NativeTunnelStartError) -> Diagnostic {
    match error {
        NativeTunnelStartError::Stopped => {
            diagnostic("ike_startup", "cancelled", "VoWiFi 启动已取消")
        }
        NativeTunnelStartError::LeaseLost => diagnostic(
            "reader",
            "reader_identity_changed",
            "读卡器或号卡租约已失效",
        ),
        NativeTunnelStartError::CertificateValidationUnavailable => diagnostic(
            "ike_certificate",
            "certificate_validation_unavailable",
            "IKE 对端证书验证不可用，未建立隧道",
        ),
        NativeTunnelStartError::Transport(_)
        | NativeTunnelStartError::Startup(TunnelStartupError::Transport(_)) => diagnostic(
            "ike_transport",
            "transport_failed",
            "IKE UDP 传输失败，请检查所选代理的 UDP 转发及 UDP 500/4500 可达性",
        ),
        NativeTunnelStartError::Startup(error) => match error {
            TunnelStartupError::Cancelled => {
                diagnostic("ike_startup", "cancelled", "VoWiFi 启动已取消")
            }
            // This outer deadline carries no finer phase proof.
            TunnelStartupError::Deadline => diagnostic(
                "ike_startup",
                "timeout",
                "IKE/EAP-AKA 隧道启动超时，请检查所选代理的 UDP 转发及 UDP 500/4500 可达性",
            ),
            TunnelStartupError::Configuration(_) => diagnostic(
                "ike_configuration",
                "configuration_invalid",
                "IKE 配置或身份参数无效",
            ),
            TunnelStartupError::SaInit(SaInitError::Timeout) => diagnostic(
                "ike_sa_init",
                "timeout",
                "IKE_SA_INIT 超时，未收到有效 UDP 500 握手响应，请检查所选代理的 UDP 转发",
            ),
            TunnelStartupError::SaInit(
                SaInitError::ProposalRejected | SaInitError::UnofferedProposal,
            ) => diagnostic(
                "ike_sa_init",
                "proposal_rejected",
                "IKE_SA_INIT 算法提议未获接受，请检查运营商算法配置",
            ),
            TunnelStartupError::SaInit(_) => diagnostic(
                "ike_sa_init",
                "negotiation_failed",
                "IKE_SA_INIT 握手响应或密钥协商失败",
            ),
            TunnelStartupError::Eap(EapHandshakeError::Rejected) => diagnostic(
                "ike_eap_aka",
                "authentication_rejected",
                "运营商拒绝 EAP-AKA 认证，请检查号卡及 VoWiFi 权限",
            ),
            TunnelStartupError::Eap(
                EapHandshakeError::Deadline | EapHandshakeError::Transport(IkeAuthError::Timeout),
            ) => diagnostic(
                "ike_eap_aka",
                "timeout",
                "IKE_SA_INIT 已完成，IKE_AUTH/EAP-AKA 认证超时",
            ),
            TunnelStartupError::Eap(EapHandshakeError::Certificate(_)) => diagnostic(
                "ike_certificate",
                "certificate_rejected",
                "IKE 对端证书验证失败，未建立隧道",
            ),
            TunnelStartupError::Eap(EapHandshakeError::Protocol(Ikev2EapError::Aka(
                EapAkaError::MacMismatch,
            ))) => diagnostic(
                "ike_eap_aka",
                "eap_mac_mismatch",
                "EAP-AKA 挑战 MAC 校验失败，请核对运营商模板的校验策略",
            ),
            TunnelStartupError::Eap(EapHandshakeError::Protocol(Ikev2EapError::Aka(
                EapAkaError::CheckcodeMismatch,
            ))) => diagnostic(
                "ike_eap_aka",
                "eap_checkcode_mismatch",
                "EAP-AKA Checkcode 校验失败，请核对运营商模板",
            ),
            TunnelStartupError::Eap(EapHandshakeError::Protocol(error)) => match error {
                Ikev2EapError::MissingIdentity => diagnostic(
                    "ike_eap_aka",
                    "eap_identity_missing",
                    "EAP 身份缺失或长度无效",
                ),
                Ikev2EapError::InvalidPacket => diagnostic(
                    "ike_eap_aka",
                    "eap_packet_invalid",
                    "ePDG 的 EAP 消息格式无效",
                ),
                Ikev2EapError::UnsupportedMethod(_) => diagnostic(
                    "ike_eap_aka",
                    "eap_method_unsupported",
                    "ePDG 请求的 EAP 方法不受支持",
                ),
                Ikev2EapError::UnsupportedAkaSubtype(subtype) => {
                    let mut value = diagnostic(
                        "ike_eap_aka",
                        "eap_aka_subtype_unsupported",
                        "ePDG 请求的 EAP-AKA 子类型不受支持",
                    );
                    value.eap_subtype = Some(*subtype);
                    value
                }
                Ikev2EapError::Aka(EapAkaError::InvalidChallenge) => diagnostic(
                    "ike_eap_aka",
                    "eap_challenge_invalid",
                    "EAP-AKA 挑战缺少有效鉴权字段",
                ),
                Ikev2EapError::Aka(EapAkaError::MissingIdentityTranscript) => diagnostic(
                    "ike_eap_aka",
                    "eap_transcript_missing",
                    "EAP-AKA 身份交换记录缺失",
                ),
                Ikev2EapError::Aka(
                    EapAkaError::InvalidResultLength | EapAkaError::InvalidAutsLength,
                ) => diagnostic("ike_eap_aka", "sim_result_invalid", "号卡 AKA 响应长度无效"),
                Ikev2EapError::Aka(_) => diagnostic(
                    "ike_eap_aka",
                    "eap_aka_attribute_invalid",
                    "EAP-AKA 挑战属性格式无效",
                ),
            },
            TunnelStartupError::Eap(EapHandshakeError::Authentication(
                EapAkaExchangeError::SimService(_),
            )) => diagnostic(
                "ike_eap_aka",
                "sim_authentication_unavailable",
                "读卡器未能完成本次 AKA 鉴权，请检查卡片会话和 ISIM/USIM 支持",
            ),
            TunnelStartupError::Eap(EapHandshakeError::Authentication(
                EapAkaExchangeError::AuthenticationRejected,
            )) => diagnostic(
                "ike_eap_aka",
                "sim_authentication_rejected",
                "号卡拒绝了本次 AKA 挑战",
            ),
            TunnelStartupError::Eap(EapHandshakeError::Authentication(_)) => diagnostic(
                "ike_eap_aka",
                "eap_response_invalid",
                "EAP-AKA 响应构造失败",
            ),
            TunnelStartupError::Eap(EapHandshakeError::Transport(
                IkeAuthError::ResponderAuthenticationFailed,
            )) => diagnostic(
                "ike_eap_aka",
                "responder_authentication_failed",
                "ePDG 响应者身份验证失败",
            ),
            TunnelStartupError::Eap(EapHandshakeError::Transport(_)) => diagnostic(
                "ike_eap_aka",
                "eap_transport_failed",
                "IKE_AUTH 传输或响应验证失败",
            ),
            TunnelStartupError::Eap(EapHandshakeError::RoundLimit) => {
                diagnostic("ike_eap_aka", "eap_round_limit", "EAP-AKA 鉴权轮次超过限制")
            }
            TunnelStartupError::Eap(EapHandshakeError::UnexpectedSuccess) => diagnostic(
                "ike_eap_aka",
                "eap_unexpected_success",
                "ePDG 的 EAP 成功响应缺少有效鉴权上下文",
            ),
            TunnelStartupError::Auth(IkeAuthError::Timeout) => {
                diagnostic("ike_auth", "timeout", "最终 IKE_AUTH/CHILD_SA 协商超时")
            }
            TunnelStartupError::Auth(IkeAuthError::PeerNotify(_)) => diagnostic(
                "ike_auth",
                "peer_rejected",
                "ePDG 拒绝最终 IKE_AUTH 或 CHILD_SA 协商",
            ),
            TunnelStartupError::Auth(IkeAuthError::ResponderAuthenticationFailed) => diagnostic(
                "ike_auth",
                "responder_authentication_failed",
                "最终 IKE_AUTH 的 ePDG 身份验证失败",
            ),
            TunnelStartupError::Auth(IkeAuthError::ChildSa(_)) => diagnostic(
                "ike_auth",
                "child_sa_invalid",
                "ePDG 返回的 CHILD_SA 参数未通过校验",
            ),
            TunnelStartupError::Auth(IkeAuthError::TrafficSelectors(_)) => diagnostic(
                "ike_auth",
                "traffic_selectors_invalid",
                "ePDG 返回的流量选择器未通过校验",
            ),
            TunnelStartupError::Auth(IkeAuthError::Sk(_) | IkeAuthError::Ike(_)) => diagnostic(
                "ike_auth",
                "ike_response_invalid",
                "最终 IKE_AUTH 响应格式或加密校验失败",
            ),
            TunnelStartupError::Auth(IkeAuthError::UnexpectedPacket) => diagnostic(
                "ike_auth",
                "unexpected_response",
                "最终 IKE_AUTH 收到非预期响应",
            ),
            TunnelStartupError::Auth(IkeAuthError::Transport(_)) => {
                diagnostic("ike_auth", "transport_failed", "最终 IKE_AUTH 网络传输失败")
            }
            TunnelStartupError::Auth(IkeAuthError::Configuration(
                TunnelConfigError::MissingOrDuplicate,
            )) => diagnostic(
                "ike_auth",
                "configuration_missing",
                "ePDG 未返回唯一有效的隧道配置",
            ),
            TunnelStartupError::Auth(IkeAuthError::Configuration(
                TunnelConfigError::MissingAddress,
            )) => diagnostic(
                "ike_auth",
                "internal_address_missing",
                "ePDG 未分配内部 IP 地址",
            ),
            TunnelStartupError::Auth(IkeAuthError::Configuration(
                TunnelConfigError::AddressOutsideSelectors,
            )) => diagnostic(
                "ike_auth",
                "internal_address_outside_selectors",
                "ePDG 分配的内部 IP 不在协商流量范围内",
            ),
            TunnelStartupError::Auth(IkeAuthError::Configuration(TunnelConfigError::Malformed)) => {
                diagnostic(
                    "ike_auth",
                    "configuration_malformed",
                    "ePDG 返回的隧道配置格式无效",
                )
            }
            TunnelStartupError::Auth(IkeAuthError::KeyDerivation(_)) => diagnostic(
                "ike_auth",
                "key_derivation_failed",
                "最终 IKE_AUTH 密钥派生失败",
            ),
            TunnelStartupError::Auth(IkeAuthError::MalformedNotify) => diagnostic(
                "ike_auth",
                "notify_invalid",
                "ePDG 返回的 Notify 消息格式无效",
            ),
            TunnelStartupError::Auth(IkeAuthError::DataPlane(_)) => diagnostic(
                "ike_auth",
                "data_plane_invalid",
                "CHILD_SA 数据通道初始化失败",
            ),
            TunnelStartupError::Auth(IkeAuthError::InvalidAttempts) => {
                diagnostic("ike_auth", "retry_policy_invalid", "IKE_AUTH 重试参数无效")
            }
            TunnelStartupError::Auth(IkeAuthError::PlannedReauthentication) => diagnostic(
                "ike_auth",
                "planned_reauthentication",
                "IKE 会话正在计划重新鉴权",
            ),
            TunnelStartupError::DataPlane(_) => {
                diagnostic("esp", "data_plane_failed", "ESP 用户态数据通道初始化失败")
            }
            TunnelStartupError::Transport(_) => unreachable!("handled above"),
        },
    }
}
fn stage(stage: RegistrationStage) -> &'static str {
    use RegistrationStage::*;
    match stage {
        Validate => "ims_configuration",
        PlainDial | PlainConnect => "ims_transport",
        InitialRegister | PlainAuthenticatedRegister => "ims_register",
        Authenticate => "ims_aka",
        PrepareSecurity | RetirePlain | InstallSecurity => "ims_security",
        ProtectedListen => "ims_incoming_listener",
        ProtectedDial | ProtectedConnect => "ims_protected_transport",
        ProtectedTemplate | ProtectedRegister => "ims_protected_register",
        Finalize => "finalize",
    }
}
fn sip(stage: &'static str, error: &SipError) -> Diagnostic {
    match error {
        SipError::RegistrationRejected(status) => {
            let mut value = if *status == 403 {
                diagnostic(
                    stage,
                    "registration_forbidden",
                    "IMS 注册被拒绝（SIP 403），请检查号卡 VoWiFi 权限与 IMS 身份配置",
                )
            } else {
                diagnostic(
                    stage,
                    "registration_rejected",
                    "IMS 服务器拒绝注册，请检查运营商与 IMS 配置",
                )
            };
            value.sip_status = Some(*status);
            value
        }
        SipError::MissingSecurityServer => diagnostic(
            "ims_security",
            "security_offer_missing",
            "IMS 未提供所需 Security-Server，未建立安全注册",
        ),
        _ => diagnostic(stage, "sip_invalid", "IMS 注册响应或事务校验失败"),
    }
}
fn registration<E>(error: &RegistrationError<E>) -> Diagnostic {
    match error {
        RegistrationError::InvalidPlan(_) => diagnostic(
            "ims_configuration",
            "configuration_invalid",
            "IMS 注册计划无效",
        ),
        RegistrationError::Cancelled(s) => diagnostic(stage(*s), "cancelled", "IMS 注册已取消"),
        RegistrationError::Deadline(s) => diagnostic(
            stage(*s),
            "timeout",
            "IMS 注册超时，请检查 P-CSCF 连通性与运营商配置",
        ),
        RegistrationError::Io { stage: s, source } => {
            let mut value = diagnostic(stage(*s), "transport_failed", "IMS 注册网络传输失败");
            value.io_operation = Some(match s {
                RegistrationStage::PlainDial => "plain_dial",
                RegistrationStage::PlainConnect => "plain_connect",
                RegistrationStage::ProtectedDial => "protected_dial",
                RegistrationStage::ProtectedConnect => "protected_connect",
                RegistrationStage::RetirePlain => "retire_plain",
                _ => "other",
            });
            value.io_kind = Some(io_kind(source.kind()));
            value.os_error = source.raw_os_error();
            value
        }
        RegistrationError::Sip { stage: s, source } => sip(stage(*s), source),
        RegistrationError::Transport { stage: s, source } => match source {
            RegisterWaitError::Sip(error) => sip(stage(*s), error),
            RegisterWaitError::Deadline => diagnostic(stage(*s), "timeout", "IMS 注册响应超时"),
            RegisterWaitError::Cancelled => diagnostic(stage(*s), "cancelled", "IMS 注册已取消"),
            RegisterWaitError::SecurityTransportUnavailable => diagnostic(
                "ims_security",
                "security_transport_unavailable",
                "IMS 安全传输不可用，未建立安全注册",
            ),
            _ => diagnostic(stage(*s), "transport_closed", "IMS 注册连接中断或不可用"),
        },
        RegistrationError::Authentication(_) => diagnostic(
            "ims_aka",
            "authentication_failed",
            "IMS AKA 认证失败，请检查当前号卡与 IMS 身份配置",
        ),
        RegistrationError::SecurityPreparation(_) => diagnostic(
            "ims_security",
            "security_offer_invalid",
            "IMS Security-Server 安全提议不受支持或校验失败，未建立安全注册",
        ),
        RegistrationError::SecurityInstall(_) => diagnostic(
            "ims_security",
            "security_install_failed",
            "IMS 安全关联安装失败，未建立安全注册",
        ),
    }
}

#[cfg(test)]
mod tests;
