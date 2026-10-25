//! Session proposal policy, matching the live Go effective-config path.
use crate::{carrier::NativeTunnelOverrides, carrier_runtime_profile::RuntimeCarrierProfile};

pub(super) fn resolve(profile: &RuntimeCarrierProfile, value: &mut NativeTunnelOverrides) {
    if value.ike_proposals.is_none() && value.esp_proposals.is_none() {
        if let Some((ike, esp)) = exact(profile) {
            value.ike_proposals = Some(ike);
            value.esp_proposals = Some(esp);
        }
    }
    value.ike_proposals.get_or_insert_with(generic_ike);
    value.esp_proposals.get_or_insert_with(generic_esp);
}
fn exact(p: &RuntimeCarrierProfile) -> Option<(Vec<String>, Vec<String>)> {
    if !p.template_ike_proposal_exact || !p.template_esp_proposal_exact || p.ike_dh_group != 14 {
        return None;
    }
    let enc = p.ike_encryption.as_deref()?.trim().to_ascii_lowercase();
    let integ = p.ike_integrity.as_deref()?.trim().to_ascii_lowercase();
    let prf = p.ike_prf.as_deref()?.trim().to_ascii_lowercase();
    let esp_enc = p.child_encryption.as_deref()?.trim().to_ascii_lowercase();
    let esp_integ = p.child_integrity.as_deref()?.trim().to_ascii_lowercase();
    if !matches!(enc.as_str(), "aes128" | "aes256")
        || !matches!(esp_enc.as_str(), "aes128" | "aes256")
        || !matches!(integ.as_str(), "sha256" | "sha512")
        || !matches!(prf.as_str(), "sha256" | "sha512")
        || !matches!(esp_integ.as_str(), "sha256" | "sha512")
    {
        return None;
    }
    Some((
        vec![format!("{enc}-{integ}-prf{prf}-modp2048")],
        vec![format!("{esp_enc}-{esp_integ}")],
    ))
}
fn generic_ike() -> Vec<String> {
    // Preserve CreateMultiProposalIKE's executable DH14 offers and order.
    // Its DH15/DH2 entries cannot execute in Go (NewDiffieHellman only accepts
    // DH14); advertising them is not retained as a working capability. This
    // applies only to absent lists, never to an explicit user proposal.
    [
        "aes256-sha512-prfsha512-modp2048",
        "aes128gcm16-prfsha256-modp2048",
        "aes256-sha256-prfsha256-modp2048",
        "aes128-sha256-prfsha256-modp2048",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
fn generic_esp() -> Vec<String> {
    [
        "aes256-sha512",
        "aes256-sha256",
        "aes256gcm16",
        "aes128gcm16",
        "aes128-sha256",
        "aes128-sha1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests;
