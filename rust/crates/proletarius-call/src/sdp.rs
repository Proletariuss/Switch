//! SDP policy: EVS Primary WB 13.2, then AMR-WB; narrowband AMR and G.711 are disabled.
use std::{collections::HashMap, fmt, net::IpAddr};

/// The negotiated AMR-WB mode set and the peer's transmit-change restrictions.
/// A mode request never authorizes a mode outside this set (RFC 4867).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmrWbModePolicy {
    modes: u16,
    period: u8,
    neighbor: bool,
}
impl AmrWbModePolicy {
    pub fn new(modes: u16, period: u8, neighbor: bool) -> Option<Self> {
        (modes != 0 && modes & !0x01ff == 0 && matches!(period, 1 | 2)).then_some(Self {
            modes,
            period,
            neighbor,
        })
    }
    pub fn single(mode: u8) -> Option<Self> {
        (mode <= 8).then(|| Self {
            modes: 1 << mode,
            period: 1,
            neighbor: false,
        })
    }
    pub fn contains(self, mode: u8) -> bool {
        mode <= 8 && self.modes & (1 << mode) != 0
    }
    pub fn modes(self) -> impl DoubleEndedIterator<Item = u8> {
        (0..=8).filter(move |mode| self.contains(*mode))
    }
    /// Frame ordinal fixes the phase of period=2 for this encoder lifetime.
    pub fn next_mode(self, current: u8, requested: Option<u8>, frame: u64) -> u8 {
        let Some(target) = requested.filter(|mode| self.contains(*mode)) else {
            return current;
        };
        if frame % u64::from(self.period) != 0 || target == current {
            return current;
        }
        if !self.neighbor {
            return target;
        }
        if target > current {
            self.modes()
                .find(|mode| *mode > current && *mode <= target)
                .unwrap_or(current)
        } else {
            self.modes()
                .rev()
                .find(|mode| *mode < current && *mode >= target)
                .unwrap_or(current)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioCodec {
    Evs {
        header_full: bool,
    },
    /// Highest compatible AMR-WB mode from the negotiated mode-set. The
    /// sender must not silently use the default 23.85 kb/s mode when the
    /// carrier restricts its answer to a lower mode.
    AmrWb {
        octet_aligned: bool,
        mode: u8,
        policy: AmrWbModePolicy,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioNegotiation {
    pub remote_ip: IpAddr,
    pub remote_port: u16,
    pub payload_type: u8,
    pub codec: AudioCodec,
    pub telephone_event_payload: Option<u8>,
    /// Negotiated RFC 4733 events 0..=15. A set bit authorizes that event.
    pub telephone_event_mask: u16,
}

/// Builds the local offer/answer used by the call runtime. Device and codec
/// availability are supplied as facts; Rust owns the SDP wire policy.
pub fn build_local_sdp(
    session_id: u64,
    local_ip: IpAddr,
    local_port: u16,
    incoming: bool,
    codec: Option<AudioCodec>,
    payload_type: Option<u8>,
    telephone_event_payload: Option<u8>,
    evs_only: bool,
) -> Result<String, SdpError> {
    build_local_sdp_version(
        session_id,
        1,
        local_ip,
        local_port,
        incoming,
        codec,
        payload_type,
        telephone_event_payload,
        evs_only,
    )
}

/// Preserve the SDP origin session ID while advancing its version for an
/// accepted in-dialog offer/answer exchange.
#[allow(clippy::too_many_arguments)]
pub fn build_local_sdp_version(
    session_id: u64,
    session_version: u64,
    local_ip: IpAddr,
    local_port: u16,
    incoming: bool,
    codec: Option<AudioCodec>,
    payload_type: Option<u8>,
    telephone_event_payload: Option<u8>,
    evs_only: bool,
) -> Result<String, SdpError> {
    build_local_sdp_version_with_events(
        session_id,
        session_version,
        local_ip,
        local_port,
        incoming,
        codec,
        payload_type,
        telephone_event_payload,
        u16::MAX,
        evs_only,
    )
}

/// Build a versioned SDP using the negotiated telephone-event set. The mask
/// precedes `evs_only`; existing builder APIs retain their default events0-15.
/// A zero mask omits telephone-event entirely, including its media-list PT.
#[allow(clippy::too_many_arguments)]
pub fn build_local_sdp_version_with_events(
    session_id: u64,
    session_version: u64,
    local_ip: IpAddr,
    local_port: u16,
    incoming: bool,
    codec: Option<AudioCodec>,
    payload_type: Option<u8>,
    telephone_event_payload: Option<u8>,
    telephone_event_mask: u16,
    evs_only: bool,
) -> Result<String, SdpError> {
    if local_port == 0 {
        return Err(SdpError::InvalidAudioEndpoint);
    }
    let family = if local_ip.is_ipv4() { "IP4" } else { "IP6" };
    let mut payloads = String::new();
    let mut attributes = String::new();
    if incoming {
        let payload = payload_type
            .filter(|value| (96..=127).contains(value))
            .ok_or(SdpError::UnsupportedAudioCodec)?;
        let codec = codec.ok_or(SdpError::UnsupportedAudioCodec)?;
        payloads.push_str(&payload.to_string());
        match codec {
            AudioCodec::Evs { header_full } => {
                let mut parameters =
                    "br=13.2;bw=wb;evs-mode-switch=0;dtx=0;cmr=-1;ch-aw-recv=-1".to_owned();
                if header_full {
                    parameters.push_str(";hf-only=1");
                }
                attributes.push_str(&format!(
                    "a=rtpmap:{payload} EVS/16000\r\na=fmtp:{payload} {parameters}\r\n"
                ));
            }
            AudioCodec::AmrWb {
                octet_aligned,
                mode,
                policy,
            } => {
                if !policy.contains(mode) {
                    return Err(SdpError::UnsupportedAudioCodec);
                }
                let aligned = if octet_aligned { "octet-align=1;" } else { "" };
                let modes = policy
                    .modes()
                    .map(|mode| mode.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                // We can receive a mode on every frame; the peer's period and
                // neighbor declarations constrain our transmitter, not theirs.
                attributes.push_str(&format!("a=rtpmap:{payload} AMR-WB/16000\r\na=fmtp:{payload} {aligned}mode-set={modes};mode-change-capability=2;max-red=0\r\n"));
            }
        }
        if let Some(dtmf) = telephone_event_payload.filter(|value| {
            telephone_event_mask != 0 && (96..=127).contains(value) && *value != payload
        }) {
            payloads.push_str(&format!(" {dtmf}"));
            attributes.push_str(&format!(
                "a=rtpmap:{dtmf} telephone-event/16000\r\na=fmtp:{dtmf} {}\r\n",
                format_event_mask(telephone_event_mask)
            ));
        }
    } else {
        payloads.push_str(if evs_only { "96" } else { "96 98" });
        if telephone_event_mask != 0 {
            payloads.push_str(" 102");
        }
        attributes.push_str("a=rtpmap:96 EVS/16000\r\na=fmtp:96 br=13.2;bw=wb;evs-mode-switch=0;dtx=0;cmr=-1;ch-aw-recv=-1\r\n");
        if !evs_only {
            // Omit mode-set because all modes and subsets are supported. This
            // permits the answer to choose a binding subset (RFC 4867 8.3.1).
            attributes.push_str("a=rtpmap:98 AMR-WB/16000\r\na=fmtp:98 octet-align=1;mode-change-capability=2;max-red=0\r\n");
        }
        if telephone_event_mask != 0 {
            attributes.push_str(&format!(
                "a=rtpmap:102 telephone-event/16000\r\na=fmtp:102 {}\r\n",
                format_event_mask(telephone_event_mask)
            ));
        }
    }
    Ok(format!(
        "v=0\r\no=- {session_id} {session_version} IN {family} {local_ip}\r\ns=Proletarius\r\nc=IN {family} {local_ip}\r\nt=0 0\r\nm=audio {local_port} RTP/AVP {payloads}\r\n{attributes}a=ptime:20\r\na=maxptime:20\r\na=sendrecv\r\n"
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdpError {
    MissingAudio,
    InvalidAudioEndpoint,
    UnsupportedAudioCodec,
}

impl fmt::Display for SdpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAudio => formatter.write_str("remote SDP has no usable audio section"),
            Self::InvalidAudioEndpoint => {
                formatter.write_str("remote SDP has an invalid numeric audio endpoint")
            }
            Self::UnsupportedAudioCodec => {
                formatter.write_str("remote SDP offers no supported EVS or AMR-WB audio codec")
            }
        }
    }
}

impl std::error::Error for SdpError {}

pub fn negotiate_audio(body: &str, answer: bool) -> Result<AudioNegotiation, SdpError> {
    negotiate_audio_with_capabilities(body, answer, true, true)
}

/// Parses an SDP offer/answer and selects only codecs the local media runtime
/// can actually encode and decode. Codec preference remains EVS, then AMR-WB.
pub fn negotiate_audio_with_capabilities(
    body: &str,
    answer: bool,
    evs_available: bool,
    amr_wb_available: bool,
) -> Result<AudioNegotiation, SdpError> {
    let available_modes: Vec<u8> = if amr_wb_available {
        (0..=8).collect()
    } else {
        Vec::new()
    };
    negotiate_audio_with_modes(body, answer, evs_available, &available_modes)
}

/// Variant for runtimes whose AMR-WB implementation has mode-specific
/// capability. The selected remote mode is accepted only when locally usable.
pub fn negotiate_audio_with_modes(
    body: &str,
    answer: bool,
    evs_available: bool,
    amr_wb_modes: &[u8],
) -> Result<AudioNegotiation, SdpError> {
    let mut session_ip = None;
    let mut media_ip = None;
    let mut audio_port = None;
    // Keep payload tokens raw as Go does. For example, `097` and `97` both
    // parse as payload type 97 but deliberately have distinct rtpmap/fmtp map
    // keys until the selected payload is converted to a numeric value.
    let mut offered = Vec::<String>::new();
    let mut rtpmap = HashMap::<String, String>::new();
    let mut fmtp = HashMap::<String, String>::new();
    // Keep malformed event fmtp out of the frozen codec parameter map while
    // ensuring it cannot authorize DTMF through the new event mask.
    let mut invalid_event_fmtp = Vec::<String>::new();
    let mut media_section = false;
    let mut audio_section = false;

    for raw in body.lines() {
        let line = raw.trim_end_matches('\r').trim();
        if line.starts_with("m=") {
            media_section = true;
            audio_section = false;
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if audio_port.is_none()
                && fields.len() > 3
                // Keep the staged parser byte-for-byte compatible with the
                // production Go call path.  In particular, Go accepts only
                // the canonical SDP media token and transport spelling here;
                // accepting a relaxed form in Rust would change live call
                // admission when this parser becomes the owner.
                && fields[0] == "m=audio"
                && fields[2] == "RTP/AVP"
                && let Some(port) = parse_go_port(fields[1])
            {
                audio_port = Some(port);
                audio_section = true;
                offered = fields[3..].iter().map(ToString::to_string).collect();
            }
            continue;
        }
        if line.starts_with("c=IN IP4 ") || line.starts_with("c=IN IP6 ") {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() == 3 {
                let parsed = fields[2].parse::<IpAddr>().ok();
                if media_section && audio_section {
                    if parsed.is_none() {
                        return Err(SdpError::InvalidAudioEndpoint);
                    }
                    media_ip = parsed;
                } else if !media_section {
                    session_ip = parsed;
                }
            }
        }
        if !audio_section {
            continue;
        }
        if let Some(value) = strip_prefix_ascii_case(line, "a=rtpmap:") {
            let mut fields = value.split_whitespace();
            if let (Some(payload), Some(mapping)) = (fields.next(), fields.next()) {
                rtpmap.insert(payload.to_owned(), mapping.to_ascii_uppercase());
            }
        } else if let Some(value) = strip_prefix_ascii_case(line, "a=fmtp:") {
            // Match Go TrimSpace followed by splitting at the first literal
            // ASCII space. Tabs before that delimiter remain part of the key.
            if let Some((payload, parameters)) = value.trim().split_once(' ') {
                fmtp.insert(payload.to_owned(), parameters.to_ascii_lowercase());
            } else if let Some(payload) = value.split_whitespace().next() {
                invalid_event_fmtp.push(payload.to_owned());
            }
        }
    }

    let remote_port = audio_port.ok_or(SdpError::MissingAudio)?;
    let remote_ip = media_ip
        .or(session_ip)
        .ok_or(SdpError::InvalidAudioEndpoint)?;

    let selected = [true, false].into_iter().find_map(|evs| {
        offered.iter().find_map(|payload| {
            let parsed = parse_go_payload(payload)?;
            if parsed < 96 || (answer && parsed != if evs { 96 } else { 98 }) {
                return None;
            }
            let mapping = rtpmap.get(payload).map(String::as_str).unwrap_or("");
            let params = fmtp.get(payload).map(String::as_str).unwrap_or("");
            let options = parse_parameters(params)?;
            if evs
                && evs_available
                && matches!(mapping, "EVS/16000" | "EVS/16000/1")
                && valid_evs(&options)
            {
                return Some((
                    parsed,
                    AudioCodec::Evs {
                        header_full: options.get("hf-only").is_some_and(|v| v == "1"),
                    },
                ));
            }
            if !evs
                && matches!(mapping, "AMR-WB/16000" | "AMR-WB/16000/1")
                && let Some((mode, octet_aligned, policy)) = supported_wb(&options)
                && policy.modes().all(|mode| amr_wb_modes.contains(&mode))
            {
                if answer && !octet_aligned {
                    return None;
                }
                return Some((
                    parsed,
                    AudioCodec::AmrWb {
                        octet_aligned,
                        mode,
                        policy,
                    },
                ));
            }
            None
        })
    });
    let (payload_type, codec) = selected.ok_or(SdpError::UnsupportedAudioCodec)?;

    let telephone_event = offered.iter().find_map(|payload| {
        let parsed = parse_go_payload(payload)?;
        let mapping = rtpmap.get(payload).map(String::as_str).unwrap_or("");
        if parsed < 96
            || parsed == payload_type
            || !matches!(mapping, "TELEPHONE-EVENT/16000" | "TELEPHONE-EVENT/16000/1")
            || (answer && parsed != 102)
        {
            return None;
        }
        let mask = if invalid_event_fmtp.contains(payload) {
            0
        } else {
            parse_event_set(fmtp.get(payload).map(String::as_str))
                .map(|events| supported_event_mask(&events))
                .unwrap_or(0)
        };
        // Preserve the original Go-parity PT projection, even when malformed
        // or unsupported events authorize no locally sendable DTMF digit.
        Some((parsed, mask))
    });
    let (telephone_event_payload, telephone_event_mask) = match telephone_event {
        Some((payload, mask)) => (Some(payload), mask),
        None => (None, 0),
    };

    Ok(AudioNegotiation {
        remote_ip,
        remote_port,
        payload_type,
        codec,
        telephone_event_payload,
        telephone_event_mask,
    })
}

/// A dialog's dynamic RTP payload assignments, including formats that were
/// advertised but not selected. Clone before preparing an exchange and retain
/// the clone only when that exchange is accepted.
#[derive(Clone, Debug, Default)]
pub struct PayloadHistory {
    assignments: HashMap<u8, PayloadIdentity>,
}

impl PayloadHistory {
    /// Admission is transactional: a conflict never reserves any new PT.
    pub fn admit_sdp(&mut self, text: &str) -> Result<(), SdpError> {
        let audio = RetainedAudio::parse(text)?;
        let mut pending = self.assignments.clone();
        for payload in audio.payloads.iter().filter(|payload| **payload >= 96) {
            let identity = audio.identity(*payload)?;
            if pending.get(payload).is_some_and(|old| old != &identity) {
                return Err(SdpError::UnsupportedAudioCodec);
            }
            pending.insert(*payload, identity);
        }
        self.assignments = pending;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PayloadIdentity {
    mapping: String,
    framing: Vec<String>,
}

/// Validate against the actual local offer retained for this exchange. The
/// legacy `answer` parser deliberately retains its original fixed-PT behavior.
pub fn negotiate_audio_answer_to_offer(
    answer: &str,
    offer: &str,
    evs_available: bool,
    amr_wb_modes: &[u8],
) -> Result<AudioNegotiation, SdpError> {
    let offered = RetainedAudio::parse(offer)?;
    let answered = RetainedAudio::parse(answer)?;
    for payload in &answered.payloads {
        if !offered.payloads.contains(payload)
            || offered.identity(*payload)? != answered.identity(*payload)?
        {
            return Err(SdpError::UnsupportedAudioCodec);
        }
        let before = offered.parameters(*payload)?;
        let after = answered.parameters(*payload)?;
        match answered.mapping(*payload)? {
            "AMR-WB/16000/1" => {
                // RFC 4867 section 8.3.1 requires every explicitly offered
                // transport parameter to remain present and unmodified. Wire
                // defaults alone do not authorize deleting an explicit value.
                if [
                    "octet-align",
                    "crc",
                    "robust-sorting",
                    "interleaving",
                    "channels",
                ]
                .iter()
                .any(|name| {
                    before
                        .get(*name)
                        .is_some_and(|value| after.get(*name) != Some(value))
                }) {
                    return Err(SdpError::UnsupportedAudioCodec);
                }
                let (_, _, before_policy) =
                    supported_wb(&before).ok_or(SdpError::UnsupportedAudioCodec)?;
                let (_, _, after_policy) =
                    supported_wb(&after).ok_or(SdpError::UnsupportedAudioCodec)?;
                if before.contains_key("mode-set")
                    && (!after.contains_key("mode-set")
                        || before_policy.modes != after_policy.modes)
                {
                    return Err(SdpError::UnsupportedAudioCodec);
                }
                // Period is a receive requirement; capability describes that
                // same participant's transmitter (RFC 4867 section 8.3.1).
                let offer_capable = before_policy.period == 2
                    || before
                        .get("mode-change-capability")
                        .is_some_and(|v| v == "2");
                if (after_policy.period == 2 && !offer_capable)
                    || (before_policy.period == 2
                        && after
                            .get("mode-change-capability")
                            .is_some_and(|v| v == "1"))
                {
                    return Err(SdpError::UnsupportedAudioCodec);
                }
            }
            "EVS/16000/1" => {
                if !evs_answer_compatible(&before, &after) {
                    return Err(SdpError::UnsupportedAudioCodec);
                }
            }
            "TELEPHONE-EVENT/16000/1" => {
                let before = offered.events(*payload)?;
                let after = answered.events(*payload)?;
                if after
                    .iter()
                    .zip(before)
                    .any(|(answer, offer)| *answer && !offer)
                {
                    return Err(SdpError::UnsupportedAudioCodec);
                }
            }
            _ => return Err(SdpError::UnsupportedAudioCodec),
        }
    }
    let selected = [true, false].into_iter().find_map(|evs| {
        answered.payloads.iter().find_map(|payload| {
            let options = answered.parameters(*payload).ok()?;
            match answered.mapping(*payload).ok()? {
                "EVS/16000/1" if evs && evs_available && valid_evs(&options) => Some((
                    *payload,
                    AudioCodec::Evs {
                        header_full: options.get("hf-only").is_some_and(|v| v == "1"),
                    },
                )),
                "AMR-WB/16000/1" if !evs => {
                    let (mode, octet_aligned, policy) = supported_wb(&options)?;
                    policy
                        .modes()
                        .all(|mode| amr_wb_modes.contains(&mode))
                        .then_some((
                            *payload,
                            AudioCodec::AmrWb {
                                octet_aligned,
                                mode,
                                policy,
                            },
                        ))
                }
                _ => None,
            }
        })
    });
    let (payload_type, codec) = selected.ok_or(SdpError::UnsupportedAudioCodec)?;
    let telephone_event = answered.payloads.iter().find_map(|payload| {
        if answered.mapping(*payload).ok()? != "TELEPHONE-EVENT/16000/1" {
            return None;
        }
        let mask = supported_event_mask(&answered.events(*payload).ok()?);
        (mask != 0).then_some((*payload, mask))
    });
    let (telephone_event_payload, telephone_event_mask) = match telephone_event {
        Some((payload, mask)) => (Some(payload), mask),
        None => (None, 0),
    };
    Ok(AudioNegotiation {
        remote_ip: answered.remote_ip,
        remote_port: answered.remote_port,
        payload_type,
        codec,
        telephone_event_payload,
        telephone_event_mask,
    })
}

/// Admission for new offers owned by the SIP runtime. The legacy parser stays
/// frozen for parity; this path validates directional EVS limits and selects
/// only a profile that the actual local answer can represent.
pub fn negotiate_audio_offer_with_modes(
    offer: &str,
    evs_available: bool,
    amr_wb_modes: &[u8],
) -> Result<AudioNegotiation, SdpError> {
    let audio = RetainedAudio::parse(offer)?;
    let selected = [true, false].into_iter().find_map(|evs| {
        audio
            .payloads
            .iter()
            .filter(|payload| **payload >= 96)
            .find_map(|payload| {
                let parameters = audio.parameters(*payload).ok()?;
                let codec = match audio.mapping(*payload).ok()? {
                    "EVS/16000/1" if evs && evs_available => {
                        retained_evs_limits(&parameters)?;
                        AudioCodec::Evs {
                            header_full: parameters
                                .get("hf-only")
                                .is_some_and(|value| value == "1"),
                        }
                    }
                    "AMR-WB/16000/1" if !evs => {
                        let (_, octet_aligned, mut policy) = supported_wb(&parameters)?;
                        if !parameters.contains_key("mode-set") {
                            let modes = amr_wb_modes
                                .iter()
                                .filter(|mode| **mode <= 8)
                                .fold(0u16, |mask, mode| mask | (1u16 << *mode));
                            policy = AmrWbModePolicy::new(modes, policy.period, policy.neighbor)?;
                        }
                        if !policy.modes().all(|mode| amr_wb_modes.contains(&mode)) {
                            return None;
                        }
                        AudioCodec::AmrWb {
                            octet_aligned,
                            mode: policy.modes().next_back()?,
                            policy,
                        }
                    }
                    _ => return None,
                };
                Some((*payload, codec))
            })
    });
    let (payload_type, codec) = selected.ok_or(SdpError::UnsupportedAudioCodec)?;
    let event = audio.payloads.iter().find_map(|payload| {
        if *payload < 96
            || *payload == payload_type
            || audio.mapping(*payload).ok()? != "TELEPHONE-EVENT/16000/1"
        {
            return None;
        }
        let mask = supported_event_mask(&audio.events(*payload).ok()?);
        (mask != 0).then_some((*payload, mask))
    });
    let (telephone_event_payload, telephone_event_mask) = match event {
        Some((payload, mask)) => (Some(payload), mask),
        None => (None, 0),
    };
    Ok(AudioNegotiation {
        remote_ip: audio.remote_ip,
        remote_port: audio.remote_port,
        payload_type,
        codec,
        telephone_event_payload,
        telephone_event_mask,
    })
}

/// Build an answer from the actual offer, preserving EVS session declarations
/// while narrowing its allowed bitrate and bandwidth to the real local profile.
/// An offerless 200 is a local offer and must use the ordinary builder instead.
#[allow(clippy::too_many_arguments)]
pub fn build_local_audio_answer_to_offer_version(
    session_id: u64,
    session_version: u64,
    local_ip: IpAddr,
    local_port: u16,
    offer: &str,
    negotiated: &AudioNegotiation,
    evs_only: bool,
) -> Result<String, SdpError> {
    let offered = RetainedAudio::parse(offer)?;
    if !offered.payloads.contains(&negotiated.payload_type)
        || negotiated.remote_ip != offered.remote_ip
        || negotiated.remote_port != offered.remote_port
    {
        return Err(SdpError::UnsupportedAudioCodec);
    }
    let mut answer = build_local_sdp_version_with_events(
        session_id,
        session_version,
        local_ip,
        local_port,
        true,
        Some(negotiated.codec),
        Some(negotiated.payload_type),
        negotiated.telephone_event_payload,
        negotiated.telephone_event_mask,
        evs_only,
    )?;
    if let AudioCodec::Evs { .. } = negotiated.codec {
        if offered.mapping(negotiated.payload_type)? != "EVS/16000/1" {
            return Err(SdpError::UnsupportedAudioCodec);
        }
        let parameters = offered.parameters(negotiated.payload_type)?;
        retained_evs_limits(&parameters).ok_or(SdpError::UnsupportedAudioCodec)?;
        let mut answer_parameters = vec!["br=13.2".to_owned(), "bw=wb".to_owned()];
        for (name, default) in [("evs-mode-switch", "0"), ("dtx", "0"), ("cmr", "-1")] {
            let value = parameters
                .get(name)
                .or_else(|| {
                    (name == "dtx")
                        .then(|| parameters.get("dtx-recv"))
                        .flatten()
                })
                .map(String::as_str)
                .unwrap_or(default);
            answer_parameters.push(format!("{name}={value}"));
        }
        answer_parameters.push("ch-aw-recv=-1".into());
        if let Some(value) = parameters.get("hf-only") {
            answer_parameters.push(format!("hf-only={value}"));
        }
        for (before, after) in [("ch-send", "ch-recv"), ("ch-recv", "ch-send")] {
            if let Some(value) = parameters.get(before) {
                answer_parameters.push(format!("{after}={value}"));
            }
        }
        if let Some(value) = parameters.get("channels") {
            answer_parameters.push(format!("channels={value}"));
        }
        let prefix = format!("a=fmtp:{} ", negotiated.payload_type);
        answer = answer
            .lines()
            .map(|line| {
                if line.starts_with(&prefix) {
                    format!("{prefix}{}", answer_parameters.join(";"))
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\r\n")
            + "\r\n";
    } else {
        // RFC 4867 requires explicitly offered transport parameters to be
        // copied unmodified, including explicit default values such as OA=0.
        let parameters = offered.parameters(negotiated.payload_type)?;
        let prefix = format!("a=fmtp:{} ", negotiated.payload_type);
        answer = answer
            .lines()
            .map(|line| {
                if let Some(existing) = line.strip_prefix(&prefix) {
                    let mut retained = existing.to_owned();
                    let existing = parse_parameters(existing).unwrap_or_default();
                    for name in [
                        "octet-align",
                        "crc",
                        "robust-sorting",
                        "interleaving",
                        "channels",
                    ] {
                        if let Some(value) = parameters.get(name)
                            && !existing.contains_key(name)
                        {
                            retained.push_str(&format!(";{name}={value}"));
                        }
                    }
                    format!("{prefix}{retained}")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\r\n")
            + "\r\n";
    }
    let modes = match negotiated.codec {
        AudioCodec::AmrWb { policy, .. } => policy.modes().collect::<Vec<_>>(),
        AudioCodec::Evs { .. } => Vec::new(),
    };
    let checked = negotiate_audio_answer_to_offer(
        &answer,
        offer,
        matches!(negotiated.codec, AudioCodec::Evs { .. }),
        &modes,
    )?;
    let same_codec = match (checked.codec, negotiated.codec) {
        (AudioCodec::Evs { header_full: a }, AudioCodec::Evs { header_full: b }) => a == b,
        (
            AudioCodec::AmrWb {
                mode: a,
                octet_aligned: oa,
                policy: pa,
            },
            AudioCodec::AmrWb {
                mode: b,
                octet_aligned: ob,
                policy: pb,
            },
        ) => a == b && oa == ob && pa.modes == pb.modes,
        _ => false,
    };
    if checked.payload_type != negotiated.payload_type
        || !same_codec
        || checked.telephone_event_payload != negotiated.telephone_event_payload
        || checked.telephone_event_mask != negotiated.telephone_event_mask
    {
        return Err(SdpError::UnsupportedAudioCodec);
    }
    Ok(answer)
}

/// Separate parser for protocol validation. Do not relax or normalize the
/// original Go-parity parser's raw payload keys and admission behavior.
struct RetainedAudio {
    remote_ip: IpAddr,
    remote_port: u16,
    payloads: Vec<u8>,
    mappings: HashMap<u8, String>,
    formats: HashMap<u8, String>,
}

impl RetainedAudio {
    fn parse(text: &str) -> Result<Self, SdpError> {
        let mut session_ip = None;
        let mut media_ip = None;
        let mut port = None;
        let mut in_media = false;
        let mut in_audio = false;
        let mut payloads = Vec::new();
        let mut mappings = HashMap::new();
        let mut formats = HashMap::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.starts_with("m=") {
                in_media = true;
                in_audio = false;
                let fields: Vec<_> = line.split_whitespace().collect();
                if port.is_none() && fields.first() == Some(&"m=audio") {
                    if fields.len() < 4 || fields[2] != "RTP/AVP" {
                        return Err(SdpError::MissingAudio);
                    }
                    port = Some(parse_go_port(fields[1]).ok_or(SdpError::InvalidAudioEndpoint)?);
                    for field in &fields[3..] {
                        let payload =
                            parse_go_payload(field).ok_or(SdpError::UnsupportedAudioCodec)?;
                        if !payloads.contains(&payload) {
                            payloads.push(payload);
                        }
                    }
                    in_audio = true;
                }
                continue;
            }
            if line.starts_with("c=IN IP4 ") || line.starts_with("c=IN IP6 ") {
                let ip = line
                    .split_whitespace()
                    .nth(2)
                    .and_then(|ip| ip.parse::<IpAddr>().ok());
                if in_audio {
                    media_ip = Some(ip.ok_or(SdpError::InvalidAudioEndpoint)?);
                } else if !in_media {
                    session_ip = ip;
                }
            }
            if !in_audio {
                continue;
            }
            if let Some(value) = strip_prefix_ascii_case(line, "a=rtpmap:") {
                let fields: Vec<_> = value.split_whitespace().collect();
                if fields.len() != 2 {
                    return Err(SdpError::UnsupportedAudioCodec);
                }
                let payload = parse_go_payload(fields[0]).ok_or(SdpError::UnsupportedAudioCodec)?;
                let mut mapping = fields[1].to_ascii_uppercase();
                if mapping.split('/').count() == 2 {
                    mapping.push_str("/1");
                }
                if mappings.get(&payload).is_some_and(|old| old != &mapping) {
                    return Err(SdpError::UnsupportedAudioCodec);
                }
                mappings.insert(payload, mapping);
            } else if let Some(value) = strip_prefix_ascii_case(line, "a=fmtp:") {
                let (payload, parameters) = value
                    .split_once(char::is_whitespace)
                    .ok_or(SdpError::UnsupportedAudioCodec)?;
                let payload = parse_go_payload(payload).ok_or(SdpError::UnsupportedAudioCodec)?;
                let parameters = parameters.trim().to_ascii_lowercase();
                if formats.get(&payload).is_some_and(|old| old != &parameters) {
                    return Err(SdpError::UnsupportedAudioCodec);
                }
                formats.insert(payload, parameters);
            }
        }
        Ok(Self {
            remote_ip: media_ip
                .or(session_ip)
                .ok_or(SdpError::InvalidAudioEndpoint)?,
            remote_port: port.ok_or(SdpError::MissingAudio)?,
            payloads,
            mappings,
            formats,
        })
    }

    fn mapping(&self, payload: u8) -> Result<&str, SdpError> {
        self.mappings
            .get(&payload)
            .map(String::as_str)
            .ok_or(SdpError::UnsupportedAudioCodec)
    }

    fn parameters(&self, payload: u8) -> Result<HashMap<String, String>, SdpError> {
        parse_parameters(self.formats.get(&payload).map(String::as_str).unwrap_or(""))
            .ok_or(SdpError::UnsupportedAudioCodec)
    }

    fn identity(&self, payload: u8) -> Result<PayloadIdentity, SdpError> {
        let mapping = self.mapping(payload)?.to_owned();
        let parameters = self.parameters(payload)?;
        let framing = match mapping.as_str() {
            "AMR-WB/16000/1" => [
                ("octet-align", "0"),
                ("crc", "0"),
                ("robust-sorting", "0"),
                ("interleaving", "0"),
                ("channels", "1"),
            ]
            .into_iter()
            .map(|(key, default)| {
                parameters
                    .get(key)
                    .map(String::as_str)
                    .unwrap_or(default)
                    .to_owned()
            })
            .collect(),
            "EVS/16000/1" => [
                ("evs-mode-switch", "0"),
                ("channels", "1"),
                ("ch-send", "1"),
                ("ch-recv", "1"),
            ]
            .into_iter()
            .map(|(key, default)| {
                parameters
                    .get(key)
                    .map(String::as_str)
                    .unwrap_or(default)
                    .to_owned()
            })
            .collect(),
            _ => Vec::new(),
        };
        Ok(PayloadIdentity { mapping, framing })
    }

    fn events(&self, payload: u8) -> Result<[bool; 256], SdpError> {
        parse_event_set(self.formats.get(&payload).map(String::as_str))
    }
}

#[derive(Clone, Copy)]
struct EvsLimits {
    // Index 0 is the SDP sender's transmit direction; index 1 is its receive
    // direction. Offer and answer indices therefore cross when compared.
    rates: [u16; 2],
    bandwidths: [u16; 2],
}

fn evs_rate_mask(value: &str) -> Option<u16> {
    const RATES: [f64; 12] = [
        5.9, 7.2, 8.0, 9.6, 13.2, 16.4, 24.4, 32.0, 48.0, 64.0, 96.0, 128.0,
    ];
    let mut parts = value.split('-');
    let lower = parts.next()?.parse::<f64>().ok()?;
    let lo = RATES.iter().position(|rate| *rate == lower)?;
    let hi = match parts.next() {
        None => lo,
        Some(value) => {
            let upper = value.parse::<f64>().ok()?;
            let hi = RATES.iter().position(|rate| *rate == upper)?;
            if hi <= lo {
                return None;
            }
            hi
        }
    };
    if parts.next().is_some() {
        return None;
    }
    Some(((1u16 << (hi + 1)) - 1) & !((1u16 << lo) - 1))
}

fn evs_bandwidth_mask(value: &str) -> Option<u16> {
    match value {
        "nb" => Some(1),
        "wb" => Some(2),
        "swb" => Some(4),
        "fb" => Some(8),
        "nb-wb" => Some(3),
        "nb-swb" => Some(7),
        "nb-fb" => Some(15),
        _ => None,
    }
}

fn evs_directional_limits(
    parameters: &HashMap<String, String>,
    name: &str,
    unrestricted: u16,
    parse: fn(&str) -> Option<u16>,
) -> Option<[u16; 2]> {
    let read = |key: &str| match parameters.get(key) {
        Some(value) => parse(value).map(Some),
        None => Some(None),
    };
    let common = read(name)?;
    let send = read(&format!("{name}-send"))?;
    let receive = read(&format!("{name}-recv"))?;
    // A common parameter and its directional forms are redundant and must
    // express identical sets (TS 26.445 A.3.1), not intersect silently.
    if common.is_some_and(|common| {
        send.is_some_and(|send| send != common) || receive.is_some_and(|receive| receive != common)
    }) {
        return None;
    }
    Some([
        send.or(common).unwrap_or(unrestricted),
        receive.or(common).unwrap_or(unrestricted),
    ])
}

fn retained_evs_limits(parameters: &HashMap<String, String>) -> Option<EvsLimits> {
    if !valid_evs(parameters)
        // The native encoder has DTX disabled and the audio bridge does not
        // yet schedule continuous CNG across DTX gaps. CMR=0 can request an
        // AMR-WB IO switch, which this fixed Primary profile cannot honour.
        || ["dtx", "dtx-recv"].iter().any(|key| {
            parameters.get(*key).is_some_and(|value| value != "0")
        })
        || parameters.get("cmr").is_some_and(|value| value != "-1")
        || parameters.get("dtx").is_some_and(|dtx| {
            parameters
                .get("dtx-recv")
                .is_some_and(|receive| receive != dtx)
        })
    {
        return None;
    }
    let rates = evs_directional_limits(parameters, "br", 0x0fff, evs_rate_mask)?;
    let bandwidths = evs_directional_limits(parameters, "bw", 0x000f, evs_bandwidth_mask)?;
    // The retained encoder profile is Primary WB 13.2. A declarative send or
    // receive constraint must leave that actual profile usable in both ways.
    (rates.iter().all(|mask| mask & (1 << 4) != 0)
        && bandwidths.iter().all(|mask| mask & (1 << 1) != 0))
    .then_some(EvsLimits { rates, bandwidths })
}

fn evs_answer_compatible(
    offered: &HashMap<String, String>,
    answered: &HashMap<String, String>,
) -> bool {
    let (Some(before), Some(after)) = (retained_evs_limits(offered), retained_evs_limits(answered))
    else {
        return false;
    };
    // Explicit session parameters must be echoed, including their presence.
    // An omitted hf-only offer permits the answer to select Header-Full-only;
    // it does not bind this PT permanently to Compact packetization.
    if ["hf-only", "evs-mode-switch", "dtx", "cmr"]
        .iter()
        .any(|key| {
            offered
                .get(*key)
                .is_some_and(|value| answered.get(*key) != Some(value))
        })
    {
        return false;
    }
    if offered
        .get("dtx-recv")
        .is_some_and(|receive| answered.get("dtx").is_some_and(|dtx| dtx != receive))
    {
        return false;
    }
    // The peer's transmit set is constrained by what we offered to receive,
    // and vice versa. Defaults are unrestricted sets, so removing a previously
    // offered limit is correctly treated as widening the negotiated range.
    (0..2).all(|direction| {
        after.rates[direction] & !before.rates[1 - direction] == 0
            && after.bandwidths[direction] & !before.bandwidths[1 - direction] == 0
    }) && answered.get("dtx").is_some_and(|value| value == "0")
        && answered.get("cmr").is_some_and(|value| value == "-1")
        && after.rates == [1 << 4; 2]
        && after.bandwidths == [1 << 1; 2]
}

fn parse_event_set(format: Option<&str>) -> Result<[bool; 256], SdpError> {
    let mut events = [false; 256];
    for range in format.unwrap_or("0-15").split(',') {
        let range = range.trim();
        let (lo, hi) = range.split_once('-').unwrap_or((range, range));
        let lo = lo
            .parse::<u8>()
            .map_err(|_| SdpError::UnsupportedAudioCodec)?;
        let hi = hi
            .parse::<u8>()
            .map_err(|_| SdpError::UnsupportedAudioCodec)?;
        if lo > hi {
            return Err(SdpError::UnsupportedAudioCodec);
        }
        events[usize::from(lo)..=usize::from(hi)].fill(true);
    }
    Ok(events)
}

fn supported_event_mask(events: &[bool; 256]) -> u16 {
    events[..16].iter().enumerate().fold(
        0u16,
        |mask, (event, enabled)| {
            if *enabled { mask | (1 << event) } else { mask }
        },
    )
}

fn format_event_mask(mask: u16) -> String {
    let mut ranges = Vec::new();
    let mut event = 0u8;
    while event < 16 {
        if mask & (1 << event) == 0 {
            event += 1;
            continue;
        }
        let start = event;
        while event < 15 && mask & (1 << (event + 1)) != 0 {
            event += 1;
        }
        ranges.push(if start == event {
            start.to_string()
        } else {
            format!("{start}-{event}")
        });
        event += 1;
    }
    ranges.join(",")
}

/// Parses an offered payload exactly as Go's `strconv.Atoi` followed by the
/// `0..=127` RTP payload range check.  The raw lexical token intentionally
/// remains the map key above.
fn parse_go_payload(value: &str) -> Option<u8> {
    value
        .parse::<i32>()
        .ok()
        .filter(|value| (0..=127).contains(value))
        .map(|value| value as u8)
}

/// Parses an SDP media port with Go's `strconv.Atoi` semantics, then applies
/// the same valid RTP port range as the live Go route.
fn parse_go_port(value: &str) -> Option<u16> {
    value
        .parse::<i32>()
        .ok()
        .filter(|value| (1..=65_535).contains(value))
        .map(|value| value as u16)
}

fn strip_prefix_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .and_then(|_| value.get(prefix.len()..))
}

fn parse_parameters(p: &str) -> Option<HashMap<String, String>> {
    let mut map = HashMap::new();
    for item in p.split(';') {
        let Some((k, v)) = item.trim().split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase());
        if map.get(&k).is_some_and(|old| old != &v) {
            return None;
        }
        map.insert(k, v);
    }
    Some(map)
}
fn valid_evs(p: &HashMap<String, String>) -> bool {
    p.iter().all(|(k, v)| match k.as_str() {
        "evs-mode-switch" => v == "0",
        "hf-only" => matches!(v.as_str(), "0" | "1"),
        "channels" | "ch-send" | "ch-recv" => v == "1",
        "cmr" | "ch-aw-recv" => matches!(v.as_str(), "-1" | "0"),
        "bw" | "bw-send" | "bw-recv" => matches!(v.as_str(), "wb" | "nb-wb" | "nb-swb" | "nb-fb"),
        "br" | "br-send" | "br-recv" => {
            let r = v.split('-').collect::<Vec<_>>();
            if r.len() > 2 {
                return false;
            }
            let Ok(lo) = r[0].parse::<f64>() else {
                return false;
            };
            let Ok(hi) = r[r.len() - 1].parse::<f64>() else {
                return false;
            };
            lo <= 13.2 && hi >= 13.2
        }
        _ => true,
    })
}
fn supported_wb(p: &HashMap<String, String>) -> Option<(u8, bool, AmrWbModePolicy)> {
    let mut modes = 0x01ff;
    let mut aligned = false;
    let mut period = 1;
    let mut neighbor = false;
    for (key, value) in p {
        match key.as_str() {
            "mode-set" => {
                modes = value.split(',').try_fold(0u16, |mask, item| {
                    let value = item.trim().parse::<i32>().ok()?;
                    (0..=8).contains(&value).then_some(mask | (1 << value))
                })?;
            }
            "octet-align" => {
                aligned = match value.as_str() {
                    "0" => false,
                    "1" => true,
                    _ => return None,
                }
            }
            "crc" | "robust-sorting" | "interleaving" if value != "0" => return None,
            "channels" if value != "1" => return None,
            "mode-change-period" => {
                period = match value.as_str() {
                    "1" => 1,
                    "2" => 2,
                    _ => return None,
                }
            }
            "mode-change-capability" if !matches!(value.as_str(), "1" | "2") => return None,
            "mode-change-neighbor" => {
                neighbor = match value.as_str() {
                    "0" => false,
                    "1" => true,
                    _ => return None,
                }
            }
            _ => {}
        }
    }
    let policy = AmrWbModePolicy::new(modes, period, neighbor)?;
    Some((policy.modes().next_back()?, aligned, policy))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn local_offer_preserves_wideband_order_and_evs_only_policy() {
        let offer = build_local_sdp(
            42,
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
            5000,
            false,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        assert!(offer.contains("m=audio 5000 RTP/AVP 96 98 102\r\n"));
        assert!(offer.contains("a=rtpmap:96 EVS/16000\r\n"));
        assert!(offer.contains("a=rtpmap:98 AMR-WB/16000\r\n"));
        assert!(!offer.contains("AMR/8000") && !offer.contains("PCMA"));
        let evs_only = build_local_sdp(
            42,
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
            5000,
            false,
            None,
            None,
            None,
            true,
        )
        .unwrap();
        assert!(evs_only.contains("m=audio 5000 RTP/AVP 96 102\r\n"));
        assert!(!evs_only.contains("AMR-WB"));
    }

    #[test]
    fn local_answer_uses_negotiated_codec_and_ipv6_endpoint() {
        let answer = build_local_sdp(
            7,
            IpAddr::V6("2001:db8::10".parse::<Ipv6Addr>().unwrap()),
            5002,
            true,
            Some(AudioCodec::AmrWb {
                octet_aligned: false,
                mode: 7,
                policy: AmrWbModePolicy::single(7).unwrap(),
            }),
            Some(97),
            Some(102),
            false,
        )
        .unwrap();
        assert!(answer.contains("c=IN IP6 2001:db8::10\r\n"));
        assert!(answer.contains("m=audio 5002 RTP/AVP 97 102\r\n"));
        assert!(answer.contains("a=fmtp:97 mode-set=7;mode-change-capability=2;max-red=0\r\n"));
        assert!(answer.contains("telephone-event/16000"));
    }

    #[test]
    fn wideband_preference_and_disabled_narrowband() {
        let s = "v=0\r\nc=IN IP4 192.0.2.8\r\nm=audio 5000 RTP/AVP 97 98 96 102\r\na=rtpmap:97 AMR/8000\r\na=rtpmap:98 AMR-WB/16000\r\na=fmtp:98 mode-set=0,1,2;octet-align=1\r\na=rtpmap:96 EVS/16000\r\na=fmtp:96 br=13.2;bw=wb\r\na=rtpmap:102 telephone-event/16000\r\n";
        for answer in [false, true] {
            let n = negotiate_audio(s, answer).unwrap();
            assert_eq!(n.payload_type, 96);
            assert_eq!(n.telephone_event_payload, Some(102));
            let n = negotiate_audio(&s.replace("EVS/16000", "PCMA/8000"), answer).unwrap();
            assert_eq!(n.payload_type, 98);
            assert_eq!(
                n.codec,
                AudioCodec::AmrWb {
                    octet_aligned: true,
                    mode: 2,
                    policy: AmrWbModePolicy::new(0b111, 1, false).unwrap(),
                }
            );
            assert!(
                negotiate_audio(
                    &s.replace("EVS/16000", "PCMA/8000")
                        .replace("AMR-WB/16000", "AMR/8000"),
                    answer
                )
                .is_err()
            );
        }
    }
    #[test]
    fn parameters_reject_unsupported_modes() {
        for p in [
            "br=24.4",
            "br=NaN",
            "bw=swb",
            "evs-mode-switch=1",
            "cmr=1",
            "ch-aw-recv=7",
        ] {
            assert!(!valid_evs(&parse_parameters(p).unwrap()));
        }
        for p in [
            "mode-set=9",
            "mode-set=",
            "mode-set=1,",
            "octet-align=2",
            "crc=1",
            "mode-change-period=0",
            "mode-change-period=3",
            "mode-change-capability=3",
            "mode-change-neighbor=2",
        ] {
            assert!(supported_wb(&parse_parameters(p).unwrap()).is_none());
        }
        assert_eq!(
            supported_wb(&parse_parameters("mode-set=7,0,3;octet-align=1").unwrap()),
            Some((
                7,
                true,
                AmrWbModePolicy::new((1 << 7) | (1 << 3) | 1, 1, false).unwrap()
            ))
        );
        assert_eq!(
            supported_wb(&parse_parameters("octet-align=0").unwrap()),
            Some((8, false, AmrWbModePolicy::new(0x01ff, 1, false).unwrap()))
        );
        assert!(parse_parameters("br=13.2;br=24.4").is_none());
    }

    #[test]
    fn wb_mode_set_and_sender_restrictions_survive_offer_answer() {
        let input = "v=0\r\nc=IN IP4 192.0.2.8\r\nm=audio 5000 RTP/AVP 98\r\na=rtpmap:98 AMR-WB/16000\r\na=fmtp:98 octet-align=1;mode-set=2,5,8;mode-change-period=2;mode-change-neighbor=1\r\n";
        let policy = AmrWbModePolicy::new((1 << 2) | (1 << 5) | (1 << 8), 2, true).unwrap();
        for answer in [false, true] {
            let n = negotiate_audio_with_modes(input, answer, false, &[2, 5, 8]).unwrap();
            assert_eq!(
                n.codec,
                AudioCodec::AmrWb {
                    octet_aligned: true,
                    mode: 8,
                    policy
                }
            );
            assert!(negotiate_audio_with_modes(input, answer, false, &[2, 8]).is_err());
            let wire = build_local_sdp(
                1,
                "192.0.2.10".parse().unwrap(),
                5001,
                true,
                Some(n.codec),
                Some(98),
                None,
                false,
            )
            .unwrap();
            assert!(wire.contains("mode-set=2,5,8;mode-change-capability=2;max-red=0"));
            // The peer's receive constraint is not reflected back as a new
            // requirement on its transmitter.
            assert!(!wire.contains("mode-change-period="));
        }
        let offer = build_local_sdp(
            1,
            "192.0.2.10".parse().unwrap(),
            5001,
            false,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        assert!(!offer.contains("mode-set="));
        assert!(offer.contains("mode-change-capability=2"));
    }

    #[test]
    fn wb_mode_changes_keep_period_phase_and_neighbor_order() {
        let policy = AmrWbModePolicy::new((1 << 2) | (1 << 5) | (1 << 8), 2, true).unwrap();
        let mut mode = 8;
        let expected = [5, 5, 2, 2, 2];
        for (frame, expected) in expected.into_iter().enumerate() {
            mode = policy.next_mode(mode, Some(2), frame as u64);
            assert_eq!(mode, expected);
        }
        assert_eq!(policy.next_mode(2, Some(8), 6), 5);
        assert_eq!(policy.next_mode(2, Some(7), 6), 2);
        assert_eq!(policy.next_mode(5, None, 8), 5);
        assert!(AmrWbModePolicy::new(0, 1, false).is_none());
        assert!(AmrWbModePolicy::new(1 << 9, 1, false).is_none());
    }

    fn exchange(payloads: &str, attributes: &str) -> String {
        format!("v=0\r\nc=IN IP4 192.0.2.8\r\nm=audio 5000 RTP/AVP {payloads}\r\n{attributes}")
    }

    fn wb_exchange(payload: u8, parameters: &str) -> String {
        exchange(
            &payload.to_string(),
            &format!("a=rtpmap:{payload} AMR-WB/16000\r\na=fmtp:{payload} {parameters}\r\n"),
        )
    }

    #[test]
    fn retained_offer_accepts_arbitrary_offered_payload_and_bandwidth_efficient_amr() {
        let offer = wb_exchange(111, "mode-set=2,5;mode-change-capability=2");
        let answer = wb_exchange(
            111,
            "mode-set=5,2;mode-change-period=2;mode-change-neighbor=1",
        );
        let n = negotiate_audio_answer_to_offer(&answer, &offer, false, &[2, 5]).unwrap();
        assert_eq!(n.payload_type, 111);
        assert_eq!(
            n.codec,
            AudioCodec::AmrWb {
                octet_aligned: false,
                mode: 5,
                policy: AmrWbModePolicy::new((1 << 2) | (1 << 5), 2, true).unwrap(),
            }
        );
        assert!(negotiate_audio_with_modes(&answer, true, false, &[2, 5]).is_err());
        assert!(negotiate_audio_answer_to_offer(&answer, &offer, false, &[5]).is_err());
        assert!(
            negotiate_audio_answer_to_offer(&answer.replace("111", "112"), &offer, false, &[2, 5])
                .is_err()
        );
    }

    #[test]
    fn retained_explicit_mode_set_cannot_be_narrowed_widened_or_omitted() {
        let offer = wb_exchange(111, "octet-align=1;mode-set=2,5");
        for parameters in [
            "octet-align=1;mode-set=2",
            "octet-align=1;mode-set=2,5,8",
            "octet-align=1",
        ] {
            assert!(
                negotiate_audio_answer_to_offer(
                    &wb_exchange(111, parameters),
                    &offer,
                    false,
                    &(0..=8).collect::<Vec<_>>()
                )
                .is_err()
            );
        }
        let unconstrained = wb_exchange(111, "octet-align=1");
        let n = negotiate_audio_answer_to_offer(&offer, &unconstrained, false, &[2, 5]).unwrap();
        assert!(matches!(n.codec, AudioCodec::AmrWb { mode: 5, .. }));
    }

    #[test]
    fn retained_answer_preserves_framing_and_rejects_unoffered_codec() {
        let offer = wb_exchange(111, "octet-align=1;mode-set=2,5");
        for answer in [
            wb_exchange(111, "mode-set=2,5"),
            wb_exchange(111, "octet-align=1;mode-set=2,5;crc=1"),
            offer.replace("AMR-WB", "EVS"),
            offer.replace("AMR-WB/16000", "AMR-WB/16000/2"),
        ] {
            assert!(negotiate_audio_answer_to_offer(&answer, &offer, true, &[2, 5]).is_err());
        }
        let offered_evs = exchange(
            "109",
            "a=rtpmap:109 EVS/16000\r\na=fmtp:109 br=13.2;bw=wb;hf-only=1;evs-mode-switch=0;dtx=0;cmr=-1\r\n",
        );
        assert!(negotiate_audio_answer_to_offer(&offered_evs, &offered_evs, true, &[]).is_ok());
        for answer in [
            offered_evs.replace("hf-only=1", "hf-only=0"),
            offered_evs.replace("br=13.2", "br=24.4"),
            offered_evs.replace("dtx=0", "dtx=1"),
            offered_evs.replace("evs-mode-switch=0", "evs-mode-switch=1"),
        ] {
            assert!(negotiate_audio_answer_to_offer(&answer, &offered_evs, true, &[]).is_err());
        }
        assert!(negotiate_audio_answer_to_offer(&offered_evs, &offered_evs, false, &[]).is_err());
    }

    #[test]
    fn retained_period_and_capability_are_directional() {
        let base = wb_exchange(111, "mode-set=2,5");
        let require_two = wb_exchange(111, "mode-set=2,5;mode-change-period=2");
        assert!(negotiate_audio_answer_to_offer(&require_two, &base, false, &[2, 5]).is_err());
        let capable = wb_exchange(111, "mode-set=2,5;mode-change-capability=2");
        assert!(negotiate_audio_answer_to_offer(&require_two, &capable, false, &[2, 5]).is_ok());
        // An offer's receive restriction does not restrict our transmitter;
        // only the answer's receive declaration belongs in our send policy.
        let n = negotiate_audio_answer_to_offer(&base, &require_two, false, &[2, 5]).unwrap();
        assert!(matches!(n.codec, AudioCodec::AmrWb { policy, .. } if policy.period == 1));
        assert!(
            negotiate_audio_answer_to_offer(
                &wb_exchange(111, "mode-set=2,5;mode-change-capability=1"),
                &require_two,
                false,
                &[2, 5]
            )
            .is_err()
        );
        assert!(
            negotiate_audio_answer_to_offer(&require_two, &require_two, false, &[2, 5]).is_ok()
        );
    }

    fn strict_evs_exchange(parameters: &str) -> String {
        exchange(
            "109",
            &format!("a=rtpmap:109 EVS/16000\r\na=fmtp:109 {parameters}\r\n"),
        )
    }

    #[test]
    fn retained_evs_answer_cannot_widen_remove_or_malformed_bitrate_bandwidth_limits() {
        let offered = strict_evs_exchange("br=13.2;bw=wb;dtx=0;cmr=-1;evs-mode-switch=0");
        assert!(negotiate_audio_answer_to_offer(&offered, &offered, true, &[]).is_ok());
        for parameters in [
            "br=5.9-128;bw=wb;dtx=0;cmr=-1;evs-mode-switch=0",
            "br=13.2;bw=nb-fb;dtx=0;cmr=-1;evs-mode-switch=0",
            "bw=wb;dtx=0;cmr=-1;evs-mode-switch=0",
            "br=13.2;dtx=0;cmr=-1;evs-mode-switch=0",
            "br=13.1-13.3;bw=wb;dtx=0;cmr=-1;evs-mode-switch=0",
            "br=13.2-13.2;bw=wb;dtx=0;cmr=-1;evs-mode-switch=0",
            "br=NaN;bw=wb;dtx=0;cmr=-1;evs-mode-switch=0",
            "br=13.2;bw=wb;bw-send=nb-wb;dtx=0;cmr=-1;evs-mode-switch=0",
            "br=13.2;br-recv=9.6-24.4;bw=wb;dtx=0;cmr=-1;evs-mode-switch=0",
        ] {
            assert!(
                negotiate_audio_answer_to_offer(
                    &strict_evs_exchange(parameters),
                    &offered,
                    true,
                    &[]
                )
                .is_err(),
                "accepted {parameters}"
            );
        }
        // The real encoder/receiver contract remains a single profile even
        // when a caller retained a broader capability offer.
        let broad = strict_evs_exchange("br=5.9-128;bw=nb-fb;dtx=0;cmr=-1;evs-mode-switch=0");
        assert!(negotiate_audio_answer_to_offer(&offered, &broad, true, &[]).is_ok());
        assert!(negotiate_audio_answer_to_offer(&broad, &broad, true, &[]).is_err());
    }

    #[test]
    fn retained_evs_directional_limits_cross_and_allow_equivalent_single_profile_answers() {
        let offered = strict_evs_exchange(
            "br-send=9.6-13.2;br-recv=13.2-24.4;bw-send=nb-wb;bw-recv=nb-fb;dtx=0;cmr=-1",
        );
        let answered =
            strict_evs_exchange("br-send=13.2;br-recv=13.2;bw-send=wb;bw-recv=wb;dtx=0;cmr=-1");
        assert!(negotiate_audio_answer_to_offer(&answered, &offered, true, &[]).is_ok());
        let common = strict_evs_exchange("br=13.2;bw=wb;dtx=0;cmr=-1");
        assert!(negotiate_audio_answer_to_offer(&common, &offered, true, &[]).is_ok());
        assert!(negotiate_audio_answer_to_offer(&answered, &common, true, &[]).is_ok());
        for parameters in [
            "br-send=24.4;br-recv=13.2;bw-send=wb;bw-recv=wb;dtx=0;cmr=-1",
            "br-send=13.2;br-recv=13.2;bw-send=swb;bw-recv=wb;dtx=0;cmr=-1",
        ] {
            assert!(
                negotiate_audio_offer_with_modes(&strict_evs_exchange(parameters), true, &[])
                    .is_err()
            );
        }
    }

    #[test]
    fn retained_evs_explicit_session_parameters_cannot_be_removed_or_changed() {
        let offered = strict_evs_exchange("br=13.2;bw=wb;dtx=0;cmr=-1;evs-mode-switch=0;hf-only=1");
        for (parameter, mutation) in [
            ("dtx=0", "dtx=1"),
            ("cmr=-1", "cmr=0"),
            ("evs-mode-switch=0", "evs-mode-switch=1"),
            ("hf-only=1", "hf-only=0"),
        ] {
            for answer in [
                offered.replace(&format!(";{parameter}"), ""),
                offered.replace(parameter, mutation),
            ] {
                assert!(
                    negotiate_audio_answer_to_offer(&answer, &offered, true, &[]).is_err(),
                    "accepted removal/change of {parameter}"
                );
            }
        }
        // Missing answer dtx/cmr implies enabled capabilities, even when those
        // parameters were absent from an unrestricted remote offer.
        let unconstrained = strict_evs_exchange("br=13.2;bw=wb");
        assert!(
            negotiate_audio_answer_to_offer(&unconstrained, &unconstrained, true, &[]).is_err()
        );
    }

    #[test]
    fn retained_evs_hf_only_can_be_added_only_when_offer_did_not_explicitly_set_it() {
        let offered = strict_evs_exchange("br=13.2;bw=wb;dtx=0;cmr=-1;evs-mode-switch=0");
        let answered = offered.replace("evs-mode-switch=0", "evs-mode-switch=0;hf-only=1");
        let n = negotiate_audio_answer_to_offer(&answered, &offered, true, &[]).unwrap();
        assert_eq!(n.codec, AudioCodec::Evs { header_full: true });
        let mut history = PayloadHistory::default();
        history.admit_sdp(&offered).unwrap();
        history.admit_sdp(&answered).unwrap();
        let explicit_zero = offered.replace("evs-mode-switch=0", "evs-mode-switch=0;hf-only=0");
        assert!(negotiate_audio_answer_to_offer(&answered, &explicit_zero, true, &[]).is_err());
        assert!(negotiate_audio_answer_to_offer(&offered, &explicit_zero, true, &[]).is_err());
        assert!(history.admit_sdp(&wb_exchange(109, "mode-set=2")).is_err());
    }

    #[test]
    fn strict_offer_answer_narrows_evs_and_preserves_explicit_zero_or_one_hf() {
        for hf in [None, Some("0"), Some("1")] {
            let mut parameters = "br-send=5.9-128;br-recv=9.6-24.4;bw-send=nb-fb;bw-recv=nb-wb;evs-mode-switch=0;dtx=0;cmr=-1;ch-send=1;ch-recv=1".to_owned();
            if let Some(hf) = hf {
                parameters.push_str(&format!(";hf-only={hf}"));
            }
            let offer = strict_evs_exchange(&parameters);
            let n = negotiate_audio_offer_with_modes(&offer, true, &[]).unwrap();
            assert_eq!(
                n.codec,
                AudioCodec::Evs {
                    header_full: hf == Some("1")
                }
            );
            let answer = build_local_audio_answer_to_offer_version(
                7,
                2,
                "192.0.2.10".parse().unwrap(),
                49170,
                &offer,
                &n,
                false,
            )
            .unwrap();
            let audio = RetainedAudio::parse(&answer).unwrap();
            let parameters = audio.parameters(109).unwrap();
            assert_eq!(parameters.get("br").map(String::as_str), Some("13.2"));
            assert_eq!(parameters.get("bw").map(String::as_str), Some("wb"));
            assert_eq!(parameters.get("hf-only").map(String::as_str), hf);
            assert_eq!(parameters.get("ch-send").map(String::as_str), Some("1"));
            assert_eq!(parameters.get("ch-recv").map(String::as_str), Some("1"));
            assert!(negotiate_audio_answer_to_offer(&answer, &offer, true, &[]).is_ok());
        }
        let offer = strict_evs_exchange("br=5.9-128;bw=nb-fb");
        let n = negotiate_audio_offer_with_modes(&offer, true, &[]).unwrap();
        let answer = build_local_audio_answer_to_offer_version(
            7,
            2,
            "192.0.2.10".parse().unwrap(),
            49170,
            &offer,
            &n,
            false,
        )
        .unwrap();
        assert!(answer.contains("dtx=0;cmr=-1"));
        assert!(negotiate_audio_answer_to_offer(&answer, &offer, true, &[]).is_ok());
    }

    #[test]
    fn strict_evs_offer_rejects_unimplemented_capabilities_and_falls_back_without_changing_legacy()
    {
        for unsupported in [
            "dtx=1",
            "dtx-recv=1",
            "cmr=0",
            "cmr=1",
            "br-send=24.4",
            "bw-recv=swb",
        ] {
            let offer = exchange(
                "109 111",
                &format!(
                    "a=rtpmap:109 EVS/16000\r\na=fmtp:109 {unsupported}\r\na=rtpmap:111 AMR-WB/16000\r\na=fmtp:111 octet-align=1;mode-set=2\r\n"
                ),
            );
            let n = negotiate_audio_offer_with_modes(&offer, true, &[2]).unwrap();
            assert_eq!(n.payload_type, 111, "no fallback for {unsupported}");
            assert!(
                negotiate_audio_offer_with_modes(&strict_evs_exchange(unsupported), true, &[2])
                    .is_err()
            );
            let answer = build_local_audio_answer_to_offer_version(
                7,
                2,
                "192.0.2.10".parse().unwrap(),
                49170,
                &offer,
                &n,
                false,
            )
            .unwrap();
            assert!(!answer.contains("EVS/16000"));
        }
        // These permissive legacy projections remain frozen. Strict owner
        // admission is intentionally provided by the separate API above.
        for parameters in ["dtx=1", "cmr=0"] {
            let n = negotiate_audio_with_modes(&strict_evs_exchange(parameters), false, true, &[2])
                .unwrap();
            assert!(matches!(n.codec, AudioCodec::Evs { .. }));
        }
    }

    #[test]
    fn strict_offer_may_select_local_amr_modes_only_when_remote_did_not_fix_a_mode_set() {
        let offer = wb_exchange(
            111,
            "octet-align=1;mode-change-period=2;mode-change-neighbor=1",
        );
        let n = negotiate_audio_offer_with_modes(&offer, false, &[2, 5]).unwrap();
        assert!(
            matches!(n.codec, AudioCodec::AmrWb { mode:5, policy, .. } if policy.modes == ((1 << 2) | (1 << 5)) && policy.period == 2 && policy.neighbor)
        );
        let answer = build_local_audio_answer_to_offer_version(
            7,
            2,
            "192.0.2.10".parse().unwrap(),
            49170,
            &offer,
            &n,
            false,
        )
        .unwrap();
        assert!(answer.contains("mode-set=2,5;mode-change-capability=2"));
        let fixed = wb_exchange(111, "octet-align=1;mode-set=0,2,5");
        assert!(negotiate_audio_offer_with_modes(&fixed, false, &[2, 5]).is_err());
    }

    #[test]
    fn strict_amr_answer_preserves_explicit_transport_defaults_and_directional_event_policy() {
        let offer = exchange(
            "111 112",
            "a=rtpmap:111 AMR-WB/16000\r\na=fmtp:111 octet-align=0;crc=0;robust-sorting=0;interleaving=0;channels=1;mode-set=2,5;mode-change-period=2;mode-change-neighbor=1\r\na=rtpmap:112 telephone-event/16000\r\na=fmtp:112 0-9\r\n",
        );
        let n = negotiate_audio_offer_with_modes(&offer, false, &[2, 5]).unwrap();
        assert_eq!(n.telephone_event_mask, 0x03ff);
        assert!(
            matches!(n.codec, AudioCodec::AmrWb { octet_aligned: false, policy, .. } if policy.period == 2 && policy.neighbor)
        );
        let answer = build_local_audio_answer_to_offer_version(
            7,
            2,
            "192.0.2.10".parse().unwrap(),
            49170,
            &offer,
            &n,
            false,
        )
        .unwrap();
        let audio = RetainedAudio::parse(&answer).unwrap();
        let parameters = audio.parameters(111).unwrap();
        for (name, value) in [
            ("octet-align", "0"),
            ("crc", "0"),
            ("robust-sorting", "0"),
            ("interleaving", "0"),
            ("channels", "1"),
        ] {
            assert_eq!(parameters.get(name).map(String::as_str), Some(value));
        }
        assert!(!parameters.contains_key("mode-change-period"));
        assert!(!parameters.contains_key("mode-change-neighbor"));
        assert!(answer.contains("a=fmtp:112 0-9\r\n"));
        assert!(!answer.contains("a=fmtp:112 0-15"));
        assert!(negotiate_audio_answer_to_offer(&answer, &offer, false, &[2, 5]).is_ok());
    }

    #[test]
    fn retained_telephone_events_must_be_an_offered_mapping_and_subset() {
        let offer = exchange(
            "111 112",
            "a=rtpmap:111 AMR-WB/16000\r\na=fmtp:111 mode-set=2\r\na=rtpmap:112 telephone-event/16000\r\na=fmtp:112 0-15\r\n",
        );
        let answer = offer.replace("0-15", "0-9,11,15");
        let n = negotiate_audio_answer_to_offer(&answer, &offer, false, &[2]).unwrap();
        assert_eq!(n.telephone_event_payload, Some(112));
        for answer in [
            offer.replace("0-15", "0-16"),
            offer.replace("0-15", "15-0"),
            offer.replace("112", "113"),
            offer.replace("telephone-event/16000", "telephone-event/8000"),
        ] {
            assert!(negotiate_audio_answer_to_offer(&answer, &offer, false, &[2]).is_err());
        }
        assert!(
            negotiate_audio_answer_to_offer(&wb_exchange(111, "mode-set=2"), &offer, false, &[2])
                .unwrap()
                .telephone_event_payload
                .is_none()
        );
    }

    #[test]
    fn history_remembers_nonselected_payloads_and_is_transactional() {
        let initial = build_local_sdp(
            1,
            "192.0.2.10".parse().unwrap(),
            5001,
            false,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        let mut history = PayloadHistory::default();
        history.admit_sdp(&initial).unwrap();
        // Initial PT96 remains EVS even if initial negotiation selected AMR.
        assert!(history.admit_sdp(&wb_exchange(96, "mode-set=2")).is_err());
        let partial = exchange(
            "110 96",
            "a=rtpmap:110 AMR-WB/16000\r\na=rtpmap:96 AMR-WB/16000\r\n",
        );
        assert!(history.admit_sdp(&partial).is_err());
        // Failure above must not reserve PT110 before discovering PT96.
        history
            .admit_sdp(&exchange("110", "a=rtpmap:110 EVS/16000\r\n"))
            .unwrap();
        assert!(
            history
                .admit_sdp(&exchange("102", "a=rtpmap:102 EVS/16000\r\n"))
                .is_err()
        );
    }

    #[test]
    fn history_accepts_policy_changes_but_requires_new_pt_for_framing() {
        let mut history = PayloadHistory::default();
        history
            .admit_sdp(&wb_exchange(111, "octet-align=1;mode-set=2,5"))
            .unwrap();
        history
            .admit_sdp(&wb_exchange(
                111,
                "octet-align=1;mode-set=0,2,5,8;mode-change-period=2;mode-change-neighbor=1",
            ))
            .unwrap();
        let mut staged = history.clone();
        staged
            .admit_sdp(&wb_exchange(112, "octet-align=0;mode-set=2"))
            .unwrap();
        assert!(
            history
                .admit_sdp(&wb_exchange(111, "octet-align=0;mode-set=2"))
                .is_err()
        );
        history
            .admit_sdp(&exchange("112", "a=rtpmap:112 EVS/16000\r\n"))
            .unwrap();
        assert!(
            staged
                .admit_sdp(&exchange("112", "a=rtpmap:112 EVS/16000\r\n"))
                .is_err()
        );
        history
            .admit_sdp(&exchange("109", "a=rtpmap:109 EVS/16000\r\n"))
            .unwrap();
        // Header-Full-only restricts the same EVS wire mapping. A subsequent
        // offer may change the restriction; offer/answer validation still
        // enforces an explicit value within each individual exchange.
        history
            .admit_sdp(&exchange(
                "109",
                "a=rtpmap:109 EVS/16000\r\na=fmtp:109 hf-only=1\r\n",
            ))
            .unwrap();
        history
            .admit_sdp(&exchange(
                "109",
                "a=rtpmap:109 EVS/16000\r\na=fmtp:109 hf-only=0\r\n",
            ))
            .unwrap();
    }

    #[test]
    fn protocol_parser_rejects_conflicting_numeric_payload_aliases() {
        let malformed = exchange(
            "111 0111",
            "a=rtpmap:111 AMR-WB/16000\r\na=rtpmap:0111 EVS/16000\r\n",
        );
        assert!(PayloadHistory::default().admit_sdp(&malformed).is_err());
        let same = exchange(
            "111 0111",
            "a=rtpmap:111 AMR-WB/16000\r\na=rtpmap:0111 AMR-WB/16000/1\r\n",
        );
        assert!(PayloadHistory::default().admit_sdp(&same).is_ok());
        // A static codec in an otherwise valid fresh offer does not make its
        // dynamic history unusable, and is never selected by the new API.
        let static_too = exchange(
            "8 111",
            "a=rtpmap:111 AMR-WB/16000\r\na=fmtp:111 mode-set=2\r\n",
        );
        assert!(PayloadHistory::default().admit_sdp(&static_too).is_ok());
    }

    #[test]
    fn telephone_event_masks_preserve_subsets_in_fresh_and_retained_negotiation() {
        let offer = exchange(
            "111 112",
            "a=rtpmap:111 AMR-WB/16000\r\na=fmtp:111 mode-set=2\r\na=rtpmap:112 telephone-event/16000\r\na=fmtp:112 0-15\r\n",
        );
        let answer = offer.replace("0-15", "0-9");
        let fresh = negotiate_audio_with_modes(&answer, false, false, &[2]).unwrap();
        let retained = negotiate_audio_answer_to_offer(&answer, &offer, false, &[2]).unwrap();
        for n in [fresh, retained] {
            assert_eq!(n.telephone_event_payload, Some(112));
            assert_eq!(n.telephone_event_mask, 0x03ff);
            assert_eq!(n.telephone_event_mask & (1 << 11), 0, "# is not negotiated");
            let wire = build_local_sdp_version_with_events(
                1,
                2,
                "192.0.2.10".parse().unwrap(),
                5001,
                true,
                Some(n.codec),
                Some(n.payload_type),
                n.telephone_event_payload,
                n.telephone_event_mask,
                false,
            )
            .unwrap();
            assert!(wire.contains("a=fmtp:112 0-9\r\n"));
            assert!(!wire.contains("a=fmtp:112 0-15"));
            assert_eq!(
                negotiate_audio_with_modes(&wire, false, false, &[2])
                    .unwrap()
                    .telephone_event_mask,
                n.telephone_event_mask
            );
        }
    }

    #[test]
    fn telephone_event_defaults_and_local_event_intersection_are_explicit() {
        let base = exchange(
            "111 102",
            "a=rtpmap:111 AMR-WB/16000\r\na=fmtp:111 mode-set=2\r\na=rtpmap:102 telephone-event/16000\r\n",
        );
        // Retain the legacy answer parser's PT98 restriction while adding
        // event masks; the fresh parser still accepts arbitrary PT111.
        assert_eq!(
            negotiate_audio_with_modes(&base, true, false, &[2]).unwrap_err(),
            SdpError::UnsupportedAudioCodec
        );
        assert_eq!(
            negotiate_audio_with_modes(&base, false, false, &[2])
                .unwrap()
                .telephone_event_mask,
            u16::MAX
        );
        let offer = format!("{base}a=fmtp:102 0-255\r\n");
        let answer = format!("{base}a=fmtp:102 2,4,11,16-255\r\n");
        let n = negotiate_audio_answer_to_offer(&answer, &offer, false, &[2]).unwrap();
        assert_eq!(n.telephone_event_mask, (1 << 2) | (1 << 4) | (1 << 11));
        assert_eq!(n.telephone_event_payload, Some(102));
        let unsupported = format!("{base}a=fmtp:102 16-255\r\n");
        let fresh = negotiate_audio_with_modes(&unsupported, false, false, &[2]).unwrap();
        assert_eq!(fresh.telephone_event_payload, Some(102));
        assert_eq!(fresh.telephone_event_mask, 0);
        let retained = negotiate_audio_answer_to_offer(&unsupported, &offer, false, &[2]).unwrap();
        assert_eq!(retained.telephone_event_payload, None);
        assert_eq!(retained.telephone_event_mask, 0);
    }

    #[test]
    fn malformed_event_list_disables_fresh_dtmf_and_rejects_retained_answer() {
        let offer = exchange(
            "111 102",
            "a=rtpmap:111 AMR-WB/16000\r\na=fmtp:111 mode-set=2\r\na=rtpmap:102 telephone-event/16000\r\na=fmtp:102 0-15\r\n",
        );
        for events in ["", "15-0", "0,", "-1", "256", "0-15;bogus=1"] {
            let answer = offer.replace("a=fmtp:102 0-15", &format!("a=fmtp:102 {events}"));
            let n = negotiate_audio_with_modes(&answer, false, false, &[2]).unwrap();
            assert_eq!(n.telephone_event_payload, Some(102));
            assert_eq!(n.telephone_event_mask, 0);
            assert!(negotiate_audio_answer_to_offer(&answer, &offer, false, &[2]).is_err());
        }
    }

    #[test]
    fn event_builder_normalizes_sparse_sets_and_omits_zero_masks() {
        let codec = AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 2,
            policy: AmrWbModePolicy::single(2).unwrap(),
        };
        let mask = (1 << 0) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 11) | (1 << 15);
        let wire = build_local_sdp_version_with_events(
            1,
            2,
            "192.0.2.10".parse().unwrap(),
            5001,
            true,
            Some(codec),
            Some(111),
            Some(112),
            mask,
            false,
        )
        .unwrap();
        assert!(wire.contains("a=fmtp:112 0,2-4,11,15\r\n"));
        assert_eq!(
            negotiate_audio_with_modes(&wire, false, false, &[2])
                .unwrap()
                .telephone_event_mask,
            mask
        );
        for incoming in [false, true] {
            let wire = build_local_sdp_version_with_events(
                1,
                2,
                "192.0.2.10".parse().unwrap(),
                5001,
                incoming,
                Some(codec),
                Some(111),
                Some(112),
                0,
                false,
            )
            .unwrap();
            assert!(!wire.contains("telephone-event"));
            let n = negotiate_audio_with_modes(&wire, false, true, &(0..=8).collect::<Vec<_>>())
                .unwrap();
            assert_eq!(n.telephone_event_payload, None);
            assert_eq!(n.telephone_event_mask, 0);
        }
    }

    #[test]
    fn legacy_telephone_event_pt_projection_survives_invalid_event_parameters() {
        let voice = "a=rtpmap:98 AMR-WB/16000\r\na=fmtp:98 octet-align=1;mode-set=2\r\n";
        for events in [
            "a=fmtp:102 nonsense\r\n",
            "a=fmtp:102 16-255\r\n",
            "a=fmtp:102 \r\n",
            "a=fmtp:102\t0-15\r\n",
        ] {
            let body = exchange(
                "98 102",
                &format!("{voice}a=rtpmap:102 telephone-event/16000\r\n{events}"),
            );
            for answer in [false, true] {
                let n = negotiate_audio_with_modes(&body, answer, false, &[2]).unwrap();
                assert_eq!(n.telephone_event_payload, Some(102), "legacy PT selection");
                assert_eq!(
                    n.telephone_event_mask, 0,
                    "invalid events cannot authorize a digit"
                );
            }
        }
        let body = exchange(
            "98 102",
            &format!("{voice}a=rtpmap:102 telephone-event/16000\r\n"),
        );
        assert_eq!(
            negotiate_audio_with_modes(&body, true, false, &[2])
                .unwrap()
                .telephone_event_mask,
            u16::MAX
        );
    }

    #[test]
    fn retained_amr_answer_cannot_delete_or_change_explicit_transport_defaults() {
        let defaults = [
            ("octet-align", "0", "1"),
            ("crc", "0", "1"),
            ("robust-sorting", "0", "1"),
            ("interleaving", "0", "1"),
            ("channels", "1", "2"),
        ];
        let parameters = defaults
            .iter()
            .map(|(name, value, _)| format!("{name}={value}"))
            .chain(std::iter::once("mode-set=2".to_owned()))
            .collect::<Vec<_>>()
            .join(";");
        let offer = wb_exchange(98, &parameters);
        assert!(negotiate_audio_answer_to_offer(&offer, &offer, false, &[2]).is_ok());
        for (name, original, modified) in defaults {
            let without = parameters
                .split(';')
                .filter(|item| item.split_once('=').map(|(key, _)| key) != Some(name))
                .collect::<Vec<_>>()
                .join(";");
            let deleted = wb_exchange(98, &without);
            assert!(
                negotiate_audio_answer_to_offer(&deleted, &offer, false, &[2]).is_err(),
                "deleted explicit {name}"
            );
            // Fresh legacy admission still accepts its previous wire defaults;
            // only retained offer/answer validation gains the presence rule.
            assert!(negotiate_audio_with_modes(&deleted, false, false, &[2]).is_ok());
            let changed = wb_exchange(
                98,
                &parameters.replace(&format!("{name}={original}"), &format!("{name}={modified}")),
            );
            assert!(
                negotiate_audio_answer_to_offer(&changed, &offer, false, &[2]).is_err(),
                "changed explicit {name}"
            );
        }
    }

    #[test]
    fn retained_amr_answer_allows_implicit_transport_defaults_when_offer_omitted_them() {
        let offer = wb_exchange(98, "mode-set=2");
        for parameters in [
            "mode-set=2",
            "octet-align=0;crc=0;robust-sorting=0;interleaving=0;channels=1;mode-set=2",
        ] {
            let answer = wb_exchange(98, parameters);
            let n = negotiate_audio_answer_to_offer(&answer, &offer, false, &[2]).unwrap();
            assert!(matches!(
                n.codec,
                AudioCodec::AmrWb {
                    octet_aligned: false,
                    mode: 2,
                    ..
                }
            ));
        }
    }
}
