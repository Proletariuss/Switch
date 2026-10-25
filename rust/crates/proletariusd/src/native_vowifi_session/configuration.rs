//! Shared, transport-free reader carrier preparation and public status preview.
//! Callers run preview on a blocking worker: IMEI persistence uses the existing
//! cross-process lock. This module never acquires a card, lease or network owner.
use super::*;
use crate::reader_session_lease::LeaseIdentity;
use proletarius_vowifi::carrier::ResolvedCarrier;
use proletarius_vowifi::carrier_overrides::CarrierOverride;
use serde_json::Value;

pub(super) struct Resolved {
    pub(super) sim: SimIdentity,
    pub(super) carrier_id: String,
    pub(super) config: proletarius_vowifi::carrier::NativeCarrierTunnelConfig,
    pub(super) runtime: proletarius_vowifi::carrier_session_config::NativeTunnelRuntimeConfig,
    pub(super) access_isim: bool,
    pub(super) ims: NativeImsRegisterProfile,
    pub(super) subscriber_identity: NativeImsSubscriberIdentity,
    pub(super) device_identity: NativeImsDeviceIdentity,
    pub(super) ike_device_identity_imei: Option<Zeroizing<String>>,
    pub(super) status: Value,
}

pub(super) fn resolve(
    identity: &LeaseIdentity,
    request_epdg: &str,
    request_port: Option<u16>,
    overrides: OverrideSet,
    device_source: impl FnOnce() -> Result<Option<Zeroizing<String>>, PlanError>,
) -> Result<Resolved, PlanError> {
    if identity.mnc.is_empty() {
        return Err(PlanError::Carrier);
    }
    let sim = SimIdentity {
        mcc: identity.mcc.clone(),
        mnc: identity.mnc.clone(),
        imsi: identity.imsi.to_string(),
        iccid: identity.iccid.to_string(),
        gid1: identity.gid1.clone(),
        gid2: identity.gid2.clone(),
        spn: identity.spn.clone(),
    };
    let registry = match CARRIER_REGISTRY.get_or_init(Registry::embedded).as_ref() {
        Ok(registry) => registry,
        Err(_) => return Err(PlanError::Carrier),
    };
    let carrier = match registry.resolve(&sim) {
        Ok(Some(carrier)) => carrier,
        Ok(None) => proletarius_vowifi::carrier::ResolvedCarrier {
            id: format!("auto-{}-{}", sim.mcc, sim.mnc),
            bundle: String::new(),
            selector: String::new(),
            carrier_name: None,
            configuration: json!({}),
        },
        Err(_) => return Err(PlanError::Carrier),
    };
    let override_value = overrides.get(&sim.mcc, &sim.mnc).cloned();
    if override_value
        .as_ref()
        .and_then(|v| v.e911.as_ref())
        .and_then(|v| v.enabled)
        == Some(true)
    {
        return Err(PlanError::Carrier);
    }
    let (mut config, runtime) = match carrier.native_session_tunnel_config(
        &sim,
        override_value.as_ref(),
        request_epdg,
        request_port,
    ) {
        Ok(value) => value,
        Err(_) => return Err(PlanError::Carrier),
    };
    let access_identity = match proletarius_vowifi::carrier_session_config::resolve_access_identity(
        &runtime.eap_identity_source,
        config.identity(),
        &identity.private_identity,
        &identity.public_identity,
        &identity.home_domain,
    ) {
        Ok(value) => value,
        Err(_) => return Err(PlanError::ReaderIdentity("reader_isim_identity_incomplete")),
    };
    let access_isim =
        runtime.eap_identity_source == "isim" && !identity.private_identity.trim().is_empty();
    if config.set_access_identity(access_identity).is_err() {
        return Err(PlanError::Carrier);
    }
    let ims = match carrier.native_ims_register_profile_with_home_domain(
        &sim,
        override_value.as_ref(),
        Some(identity.home_domain.as_str()),
        access_isim,
    ) {
        Ok(profile) => profile,
        Err(_) => return Err(PlanError::Carrier),
    };
    let subscriber_identity = match resolve_session_subscriber_identity(&ims, identity) {
        Ok(identity) => identity,
        Err(error) => return Err(PlanError::SubscriberIdentity(error)),
    };
    let persisted = device_source()?;
    let persisted_imei = persisted.as_deref().map(String::as_str);
    let device_identity = ims
        .resolve_device_identity(persisted_imei)
        .map_err(PlanError::DeviceIdentity)?;
    // Match the deployed Go IKE identity gate independently from SIP identity:
    // explicit false suppresses the optional notify; an explicit carrier IMEI
    // wins over the existing persisted one. Both sources were validated above
    // or by the identity store, and this copy is zeroized with the session.
    let ike_device_identity_imei = if override_value
        .as_ref()
        .and_then(|value| value.device_identity_enabled)
        == Some(false)
    {
        None
    } else {
        override_value
            .as_ref()
            .and_then(|value| value.device_identity_imei.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or(persisted_imei)
            .map(|value| Zeroizing::new(value.to_owned()))
    };
    let status = projection(
        &sim,
        &carrier,
        &config,
        &runtime,
        &ims,
        &subscriber_identity,
        override_value.as_ref(),
        persisted_imei,
    );
    Ok(Resolved {
        sim,
        carrier_id: carrier.id,
        config,
        runtime,
        access_isim,
        ims,
        subscriber_identity,
        device_identity,
        ike_device_identity_imei,
        status,
    })
}

/// Sanitized typed failure with the facts already known before resolution failed.
/// Subscriber credentials and template contents are never included in Debug.
pub(crate) struct Failure {
    code: &'static str,
    status: Value,
}
impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Failure")
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}
impl Failure {
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }
    pub(crate) fn status_projection(&self) -> Value {
        self.status.clone()
    }
}

pub(crate) fn preview_reader_status(
    snapshot: &crate::reader_lifecycle::Snapshot,
    data_root: &Path,
) -> Result<Value, Failure> {
    preview_reader_status_with_path(snapshot, data_root, || config_path(data_root))
}

fn preview_reader_status_with_path(
    snapshot: &crate::reader_lifecycle::Snapshot,
    data_root: &Path,
    source_path: impl FnOnce() -> Result<PathBuf, PlanError>,
) -> Result<Value, Failure> {
    let Some(metadata) = snapshot.usim_info.as_ref() else {
        return Ok(json!({"available":false}));
    };
    let identity = snapshot_identity(metadata);
    let mut status = json!({"available":true,"mcc":identity.mcc,"mnc":identity.mnc,
        "imsi":identity.imsi.as_str()});
    let result = (|| {
        let imei = Zeroizing::new(
            crate::reader_device_identity::Store::new(data_root)
                .resolve_or_create()
                .map_err(PlanError::DeviceIdentityConfig)?,
        );
        status["imei"] = json!(imei.as_str());
        let path = source_path()?;
        status["carrier_config_path"] = path_status_value(&path);
        let overrides = read_overrides(&path)?;
        if overrides
            .get(&identity.mcc, &identity.mnc)
            .and_then(|value| value.e911.as_ref())
            .and_then(|value| value.enabled)
            == Some(true)
        {
            status["e911_enabled"] = json!(true);
            status["e911_status"] = json!("unsupported");
        }
        resolve(&identity, "", None, overrides, || Ok(Some(imei)))
    })();
    match result {
        Ok(prepared) => {
            let mut projection = prepared.status;
            projection["carrier_config_path"] = status["carrier_config_path"].clone();
            Ok(projection)
        }
        Err(error) => {
            status["carrier_config_error"] = json!(error.code());

            Err(Failure {
                code: error.code(),
                status,
            })
        }
    }
}

/// JSON requires UTF-8, while a valid Unix filename need not be UTF-8.
/// Only the public display value is lossy; file reads retain the original path.
pub(super) fn path_status_value(path: &Path) -> Value {
    json!(path.to_string_lossy())
}

pub(crate) fn live_status(context: &NativeSessionContext) -> Value {
    context.configuration_status.clone()
}

// Freeze only the selected source path at daemon startup. Each status/start
// still reads its current contents; a live session keeps its prepared snapshot.
static STARTUP_CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn initialize_config_path(
    arguments: &crate::daemon_args::DaemonArgs,
    data_root: &Path,
) -> Result<(), PlanError> {
    let environment = env::var_os(OVERRIDES_ENVIRONMENT);
    let path = resolve_path(
        arguments.vowifi_config.as_deref(),
        environment.as_deref(),
        || env::current_dir().map_err(|_| PlanError::Overrides),
        || Ok(data_root.to_owned()),
    )?;
    STARTUP_CONFIG_PATH
        .set(path)
        .map_err(|_| PlanError::Overrides)
}

pub(super) fn config_path(data_root: &Path) -> Result<PathBuf, PlanError> {
    config_path_with_default(|| Ok(data_root.to_owned()))
}

pub(super) fn config_path_with_default(
    default_root: impl FnOnce() -> Result<PathBuf, PlanError>,
) -> Result<PathBuf, PlanError> {
    if let Some(path) = STARTUP_CONFIG_PATH.get() {
        return Ok(path.clone());
    }
    // Direct service/unit-test construction does not run the daemon entry
    // point. Preserve its environment/default injection without parsing the
    // test harness's arguments or changing process-global environment state.
    let environment = env::var_os(OVERRIDES_ENVIRONMENT);
    resolve_path(
        None,
        environment.as_deref(),
        || env::current_dir().map_err(|_| PlanError::Overrides),
        default_root,
    )
}

fn resolve_path(
    explicit: Option<&std::ffi::OsStr>,
    environment: Option<&std::ffi::OsStr>,
    working_directory: impl FnOnce() -> Result<PathBuf, PlanError>,
    default_root: impl FnOnce() -> Result<PathBuf, PlanError>,
) -> Result<PathBuf, PlanError> {
    fn nonempty(value: Option<&std::ffi::OsStr>) -> Option<&std::ffi::OsStr> {
        value
            .map(crate::daemon_args::trimmed)
            .filter(|value| !value.is_empty())
    }
    let selected = nonempty(explicit).or_else(|| nonempty(environment));
    let path = match selected {
        Some(value) => PathBuf::from(value),
        None => default_root()?.join("carrier-overrides.yaml"),
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        let cwd = working_directory()?;
        if !cwd.is_absolute() {
            return Err(PlanError::Overrides);
        }
        cwd.join(path)
    };
    // Go filepath.Abs/Clean are lexical. Do not canonicalize: nonexistent
    // files are valid empty overrides, and no filesystem lookup is needed.
    let mut clean = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                clean.pop();
            }
            component => clean.push(component.as_os_str()),
        }
    }
    Ok(clean)
}

pub(super) fn read_overrides(path: &Path) -> Result<OverrideSet, PlanError> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OverrideSet::default());
        }
        Err(_) => return Err(PlanError::Overrides),
    };
    if data.iter().all(u8::is_ascii_whitespace) {
        return Ok(OverrideSet::default());
    }
    OverrideSet::parse(&data).map_err(|_| PlanError::Overrides)
}

fn snapshot_identity(value: &Value) -> LeaseIdentity {
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    LeaseIdentity {
        iccid: Zeroizing::new(text("iccid")),
        imsi: Zeroizing::new(text("imsi")),
        mcc: text("mcc"),
        mnc: text("mnc"),
        gid1: text("gid1"),
        gid2: text("gid2"),
        spn: if text("operator_source") == "ef_spn" {
            text("operator_name")
        } else {
            String::new()
        },
        private_identity: Zeroizing::new(text("private_identity")),
        public_identity: Zeroizing::new(text("public_identity")),
        home_domain: Zeroizing::new(text("home_domain")),
        phone_number: Zeroizing::new(text("phone_number")),
        smsc: Zeroizing::new(text("smsc")),
        smsc_psi: Zeroizing::new(text("smsc_psi")),
        smsc_target: Zeroizing::new(text("smsc_target")),
    }
}

fn projection(
    sim: &SimIdentity,
    carrier: &ResolvedCarrier,
    config: &proletarius_vowifi::carrier::NativeCarrierTunnelConfig,
    runtime: &proletarius_vowifi::carrier_session_config::NativeTunnelRuntimeConfig,
    ims: &NativeImsRegisterProfile,
    subscriber: &NativeImsSubscriberIdentity,
    override_value: Option<&CarrierOverride>,
    persisted_imei: Option<&str>,
) -> Value {
    let imei = override_value
        .and_then(|value| value.device_identity_imei.as_deref())
        .or(persisted_imei);
    let algorithm_policy = override_value
        .and_then(|value| value.algorithm_policy.as_deref())
        .unwrap_or("balanced");
    let aka = match config.aka_challenge_mode {
        proletarius_ike::eap_aka::AkaCheckcodeMode::Minimal => "minimal",
        proletarius_ike::eap_aka::AkaCheckcodeMode::Omit => "omit",
        proletarius_ike::eap_aka::AkaCheckcodeMode::Checkcode => "checkcode",
        proletarius_ike::eap_aka::AkaCheckcodeMode::Recompute => "recompute",
    };
    json!({"available":true,"mcc":sim.mcc,"mnc":sim.mnc,"imsi":sim.imsi,"imei":imei,
        "epdg_fqdn":config.epdg,"epdg_source":config.epdg_source,"profile_id":carrier.id,
        "profile_name":runtime.profile.name,"profile_source":runtime.profile.source,
        "override_matched":override_value.is_some(),"override_id":runtime.override_id,
        "effective_apn":config.apn,"ip_stack":ip_stack_name(config.ip_stack),
        "algorithm_policy":algorithm_policy,"aka_challenge_mode":aka,
        "device_model":override_value.and_then(|value| value.device_model.as_deref()).unwrap_or("iPhone"),
        "ims_transport":ims.transport().as_str(),
        "ims_identity_source":if subscriber.uses_isim(){"isim"} else {"derived"},
        "eap_identity_source":runtime.eap_identity_source,"e911_enabled":false,
        "carrier_config_error":""})
}

#[cfg(test)]
mod tests;
