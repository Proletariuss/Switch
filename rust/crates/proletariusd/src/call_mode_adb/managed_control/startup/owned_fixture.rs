//! Genuine original writer plus a private inert remote signing peer.
//! No key getter, deserialization constructor or production path exists.
use super::*;
use switch_voice_supervisor::Exit;

pub(in crate::call_mode_adb) struct OriginalSigningPeer {
    binding: DurableControlBinding,
    key: Zeroizing<[u8; 32]>,
}
#[derive(Clone, Copy)]
pub(in crate::call_mode_adb) enum SignedFault {
    None,
    BadMac,
    BadNonce,
}
impl OriginalSigningPeer {
    pub(in crate::call_mode_adb) fn create(
        root: &Path,
        proof: &crate::daemon_lifetime::RootProof,
        producer: &str,
        generation: u64,
    ) -> Result<Self, ProbeError> {
        // Real random key/session, durable record and actual original writer FD.
        let original = ManagedControl::create(root, proof, producer, generation)?;
        let peer = Self {
            binding: original.binding(),
            key: Zeroizing::new(*original.key),
        };
        // Startup capture must acquire the genuine record's OFD pin anew.
        drop(original);
        Ok(peer)
    }
    pub(in crate::call_mode_adb) fn binding(&self) -> &DurableControlBinding {
        &self.binding
    }
    pub(in crate::call_mode_adb) fn pid_path(&self) -> String {
        format!(
            "{MODULE_VOICE_REMOTE_DIRECTORY}/control-{}/child.pid",
            self.binding.session
        )
    }
    pub(in crate::call_mode_adb) fn response(
        &self,
        command: ControlCommand,
        nonce: &str,
        state: ControlState,
        pid: u32,
        exit: Exit,
        fault: SignedFault,
    ) -> Result<String, ProbeError> {
        if nonce.len() != 64 {
            return Err(failure());
        }
        let mut wrong_nonce = nonce.as_bytes().to_vec();
        wrong_nonce[0] = if wrong_nonce[0] == b'0' { b'1' } else { b'0' };
        let wrong_nonce = String::from_utf8(wrong_nonce).map_err(io_error)?;
        let response_nonce = if matches!(fault, SignedFault::BadNonce) {
            &wrong_nonce
        } else {
            nonce
        };
        let mut signed = switch_voice_supervisor::encode_response(
            &self.binding.session,
            response_nonce,
            command,
            state,
            pid,
            exit,
            &self.key[..],
        )
        .map_err(io_error)?;
        if matches!(fault, SignedFault::BadMac) {
            // Change a single authenticated byte; preserve the wire grammar.
            if signed.pop() != Some('\n') {
                return Err(failure());
            }
            let last = signed.pop().ok_or_else(failure)?;
            signed.push(if last == '0' { '1' } else { '0' });
            signed.push('\n');
        }
        Ok(signed)
    }
    pub(in crate::call_mode_adb) fn matches_command(
        &self,
        command: &str,
        nonce: &str,
        word: &str,
    ) -> bool {
        let remote = format!(
            "{MODULE_VOICE_REMOTE_DIRECTORY}/control-{}",
            self.binding.session
        );
        command
            == format!(
                "'{SUPERVISOR}' client --socket '{remote}/socket' --key-file '{remote}/key' --session '{}' --nonce '{nonce}' --command {word}",
                self.binding.session
            )
    }
}
