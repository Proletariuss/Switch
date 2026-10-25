//! Module selection from CoreAudio facts. DeviceUID is opaque: never parse it
//! or use model/name/transport equality as proof of a physical relationship.
use std::{collections::BTreeSet, ffi::c_void};

const USB: u32 = u32::from_be_bytes(*b"usb ");
const MAX_DEVICES: usize = 256;

#[derive(Clone)]
struct Device {
    id: u32,
    name: String,
    uid: String,
    transport: u32,
    inputs: u32,
    outputs: u32,
    // None means the CoreAudio property was unavailable, not an empty group.
    related: Option<Vec<u32>>,
}

#[derive(Default)]
struct Observations {
    devices: Vec<Device>,
    invalid: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Input,
    Output,
}

fn role(name: &str) -> Result<Option<Role>, ()> {
    let name = name.to_lowercase();
    let output = name.contains("as interface");
    let input =
        name.contains("ac interface") || name.contains("baiwang") || name.contains("quectel");
    // AS endpoints can also contain the manufacturer's name. An explicit AC
    // and AS combination, however, does not identify a unique endpoint role.
    if output && name.contains("ac interface") {
        return Err(());
    }
    Ok(if output {
        Some(Role::Output)
    } else if input {
        Some(Role::Input)
    } else {
        None
    })
}

fn group(device: &Device) -> Option<BTreeSet<u32>> {
    let related = device.related.as_ref()?;
    let mut members = BTreeSet::new();
    for id in related {
        if *id == 0 || !members.insert(*id) {
            return None;
        }
    }
    members.insert(device.id);
    Some(members)
}

impl Observations {
    fn select(&self, speaker: u32, microphone: u32) -> Option<(u32, u32)> {
        if self.invalid || self.devices.len() > MAX_DEVICES {
            return None;
        }
        let mut ids = BTreeSet::new();
        let mut uids = BTreeSet::new();
        for device in &self.devices {
            if device.id == 0
                || !ids.insert(device.id)
                || (!device.uid.is_empty() && !uids.insert(device.uid.as_str()))
            {
                return None;
            }
        }
        let find = |id| self.devices.iter().find(|device| device.id == id);
        let mut candidates = Vec::new();
        for device in &self.devices {
            if let Some(role) = role(&device.name).ok()? {
                if device.uid.trim().is_empty()
                    || device.transport != USB
                    || match role {
                        Role::Input => device.inputs == 0,
                        Role::Output => device.outputs == 0,
                    }
                {
                    return None;
                }
                candidates.push((device, role));
            }
        }
        // No implicit active-module choice: two complete modules are ambiguous.
        if candidates.len() != 2 {
            return None;
        }
        let input = candidates.iter().find(|(_, role)| *role == Role::Input)?.0;
        let output = candidates.iter().find(|(_, role)| *role == Role::Output)?.0;
        let members = group(input)?;
        if input.id == output.id || group(output)? != members || !members.contains(&output.id) {
            return None;
        }
        for id in &members {
            let peer = find(*id)?;
            if peer.uid.trim().is_empty() || peer.transport != USB || group(peer)? != members {
                return None;
            }
        }
        let speaker = find(speaker)?;
        let microphone = find(microphone)?;
        if speaker.uid.trim().is_empty()
            || microphone.uid.trim().is_empty()
            || speaker.outputs == 0
            || microphone.inputs == 0
        {
            return None;
        }
        let shares_group = |device: &Device| {
            members.contains(&device.id)
                || device
                    .related
                    .as_ref()
                    .is_some_and(|related| related.iter().any(|id| members.contains(id)))
        };
        crate::route_is_safe(
            speaker.id,
            microphone.id,
            input.id,
            output.id,
            shares_group(speaker),
            shares_group(microphone),
        )
        .then_some((input.id, output.id))
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn switch_audio_module_devices_create() -> *mut c_void {
    Box::into_raw(Box::new(Observations::default())).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_module_devices_destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Observations>()) });
    }
}

/// Copies a bounded observation. A bad observation poisons this snapshot so
/// callers cannot accidentally select from a partially accepted enumeration.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_module_devices_add(
    handle: *mut c_void,
    id: u32,
    name: *const u8,
    name_len: usize,
    uid: *const u8,
    uid_len: usize,
    transport: u32,
    inputs: u32,
    outputs: u32,
    related: *const u32,
    related_len: usize,
    related_known: u8,
) -> u8 {
    let Some(snapshot) = (unsafe { handle.cast::<Observations>().as_mut() }) else {
        return 0;
    };
    if snapshot.invalid {
        return 0;
    }
    let valid = name_len <= 1024
        && uid_len <= 1024
        && related_len <= MAX_DEVICES
        && snapshot.devices.len() < MAX_DEVICES
        && related_known <= 1
        && (name_len == 0 || (!name.is_null() && (name as usize).checked_add(name_len).is_some()))
        && (uid_len == 0 || (!uid.is_null() && (uid as usize).checked_add(uid_len).is_some()))
        && (related_len == 0
            || (!related.is_null()
                && (related as usize).is_multiple_of(align_of::<u32>())
                && (related as usize)
                    .checked_add(related_len * size_of::<u32>())
                    .is_some()))
        && (related_known != 0 || related_len == 0);
    if !valid {
        snapshot.invalid = true;
        return 0;
    }
    let name = if name_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(name, name_len) }
    };
    let uid = if uid_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(uid, uid_len) }
    };
    let (Ok(name), Ok(uid)) = (std::str::from_utf8(name), std::str::from_utf8(uid)) else {
        snapshot.invalid = true;
        return 0;
    };
    if name.contains('\0') || uid.contains('\0') {
        snapshot.invalid = true;
        return 0;
    }
    let related = if related_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(related, related_len) }.to_vec()
    };
    snapshot.devices.push(Device {
        id,
        name: name.into(),
        uid: uid.into(),
        transport,
        inputs,
        outputs,
        related: (related_known != 0).then_some(related),
    });
    1
}

/// Packed input ID in the high 32 bits, output ID in the low 32 bits; zero is
/// unconfirmed. The handle is confined to one synchronous Swift enumeration.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_audio_module_devices_select(
    handle: *const c_void,
    speaker: u32,
    microphone: u32,
) -> u64 {
    unsafe { handle.cast::<Observations>().as_ref() }
        .and_then(|snapshot| snapshot.select(speaker, microphone))
        .map_or(0, |(input, output)| {
            (u64::from(input) << 32) | u64::from(output)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device(id: u32, name: &str, inputs: u32, outputs: u32, related: &[u32]) -> Device {
        Device {
            id,
            name: name.into(),
            uid: format!("opaque:{id}"),
            transport: USB,
            inputs,
            outputs,
            related: Some(related.to_vec()),
        }
    }
    fn snapshot() -> Observations {
        Observations {
            devices: vec![
                device(1, "AC Interface", 1, 0, &[1, 2]),
                device(2, "AS Interface", 0, 1, &[2, 1]),
                device(10, "Mac speaker", 0, 2, &[]),
                device(11, "Mac mic", 1, 0, &[]),
            ],
            invalid: false,
        }
    }
    #[test]
    fn same_group_selection_is_independent_of_enumeration_and_related_order() {
        let mut s = snapshot();
        assert_eq!(s.select(10, 11), Some((1, 2)));
        s.devices.reverse();
        assert_eq!(s.select(10, 11), Some((1, 2)));
    }
    #[test]
    fn two_modules_and_cross_group_pairs_are_unconfirmed() {
        let mut s = snapshot();
        s.devices.extend([
            device(3, "AS Interface", 0, 1, &[3, 4]),
            device(4, "AC Interface", 1, 0, &[4, 3]),
        ]);
        assert_eq!(s.select(10, 11), None);
        s.devices.reverse();
        assert_eq!(s.select(10, 11), None);
        let mut s = snapshot();
        s.devices[0].related = Some(vec![1]);
        s.devices[1].related = Some(vec![2]);
        assert_eq!(s.select(10, 11), None);
    }
    #[test]
    fn absent_duplicate_and_conflicting_identity_fail_closed() {
        for index in [0, 1, 2, 3] {
            let mut s = snapshot();
            s.devices[index].uid.clear();
            assert_eq!(s.select(10, 11), None);
        }
        let mut s = snapshot();
        s.devices[1].uid = s.devices[0].uid.clone();
        assert_eq!(s.select(10, 11), None);
        let mut s = snapshot();
        s.devices[0].related = None;
        assert_eq!(s.select(10, 11), None);
        let mut s = snapshot();
        s.devices[1].related = Some(vec![2]);
        assert_eq!(s.select(10, 11), None);
        let mut s = snapshot();
        s.devices[0].related = Some(vec![1, 2, 99]);
        assert_eq!(s.select(10, 11), None);
        let mut s = snapshot();
        s.devices[1].related = Some(vec![1, 2, 2]);
        assert_eq!(s.select(10, 11), None);
    }
    #[test]
    fn wrong_direction_unknown_transport_and_ambiguous_name_fail_closed() {
        for index in [0, 1] {
            let mut s = snapshot();
            s.devices[index].inputs = 0;
            s.devices[index].outputs = 0;
            assert_eq!(s.select(10, 11), None);
        }
        let mut s = snapshot();
        s.devices[1].transport = 0;
        assert_eq!(s.select(10, 11), None);
        let mut s = snapshot();
        s.devices[0].name = "AC Interface AS Interface".into();
        assert_eq!(s.select(10, 11), None);
    }
    #[test]
    fn defaults_on_any_related_endpoint_cannot_loop_back() {
        let mut s = snapshot();
        for d in &mut s.devices[..2] {
            d.related = Some(vec![1, 2, 3]);
        }
        s.devices
            .push(device(3, "Other module endpoint", 1, 1, &[3, 2, 1]));
        assert_eq!(s.select(10, 11), Some((1, 2)));
        assert_eq!(s.select(3, 11), None);
        assert_eq!(s.select(10, 3), None);
        s.devices[2].related = Some(vec![1]);
        assert_eq!(s.select(10, 11), None);
    }
    #[test]
    fn ffi_bad_observation_poisoning_and_null_handles_are_safe() {
        unsafe {
            assert_eq!(
                switch_audio_module_devices_select(std::ptr::null(), 10, 11),
                0
            );
            switch_audio_module_devices_destroy(std::ptr::null_mut());
            let h = switch_audio_module_devices_create();
            assert_eq!(
                switch_audio_module_devices_add(
                    h,
                    1,
                    std::ptr::null(),
                    1,
                    std::ptr::null(),
                    0,
                    USB,
                    1,
                    0,
                    std::ptr::null(),
                    0,
                    0
                ),
                0
            );
            assert_eq!(switch_audio_module_devices_select(h, 10, 11), 0);
            switch_audio_module_devices_destroy(h);
        }
    }
}
