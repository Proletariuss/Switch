//! Call configuration comes from the successful REGISTER owner, never from a
//! response DTO or a newly selected P-CSCF/security endpoint.
use crate::native_ims_calls::Config;
use proletarius_vowifi::carrier_runtime_profile::RuntimeCarrierProfile;
use proletarius_vowifi::ims_profile::NativeImsRegisterProfile;
use proletarius_vowifi::registration_coordinator::RegisteredImsSession;

pub(crate) fn prepare(
    session: &RegisteredImsSession,
    profile: &NativeImsRegisterProfile,
    carrier: &RuntimeCarrierProfile,
    sms_only: bool,
) -> Result<Config, String> {
    let context = session.call_context().map_err(str::to_owned)?;
    if context.carrier_id() != profile.carrier_id()
        || context.carrier_id() != carrier.id
        || context.domain() != profile.domain()
        || context.realm() != profile.realm()
        || context.phone_context() != profile.phone_context()
        || context.template_id() != profile.template().id()
        || context.port_c() == 0
        || context.port_s() == 0
        || context.register_cseq() == 0
        || context.contact().is_empty()
    {
        return Err("registered_call_profile_mismatch".into());
    }
    // The first valid P-Associated-URI E.164 number is the registrar-authorized
    // originating number. Without it, preserve the configured IMPU verbatim.
    let (from_uri, preferred_identity) = match context.routing().registered_msisdn() {
        Some(number) => (
            format!("sip:{number}@{};user=phone", context.domain()),
            format!("tel:{number}"),
        ),
        None => (
            context.public_identity().to_owned(),
            context.public_identity().to_owned(),
        ),
    };
    Ok(Config {
        local_ip: context.local_ip(),
        port_c: context.port_c(),
        contact: context.contact().to_owned(),
        from_uri,
        preferred_identity,
        route: format!("<sip:{};lr>", context.peer()),
        service_routes: context.routing().routes().to_vec(),
        encoded_icsi: context.encoded_icsi().to_owned(),
        phone_context: context.phone_context().to_owned(),
        home_domain: context.domain().to_owned(),
        realm: context.realm().to_owned(),
        user_agent: context.user_agent().to_owned(),
        security_verify: context.security_verify().unwrap_or_default().to_owned(),
        pani: context.pani().to_owned(),
        early_media_header: carrier.support_p_early_media && carrier.early_media_needs_header,
        // Preserve production EVS/AMR-WB negotiation. The Go EVS-only flag was
        // a link-time interoperability test option, not a carrier capability.
        evs_only: false,
        sms_only,
        ussd_service_selector: context.template_id() == "3_hk",
    })
}

#[cfg(test)]
mod tests;
