//! `ReprogControlsV4` (`0x1B04`) control remapping.

use agnoperiph_core::{
    CoreError, CoreResult, KeyCode, KeyIndex, KeymapCapabilities, KeymapController, KeymapSnapshot,
};
use agnoperiph_transport::HidTransport;

use crate::LogitechHidppDevice;
use crate::protocol::{KEY_FLAG_REPROGRAMMABLE, feature};

/// One control reported by `ReprogControlsV4` (`0x1B04`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LogitechControlInfo {
    pub cid: u16,
    pub task_id: u16,
    /// `getCidInfo` byte 4 (low) combined with byte 8 (high).
    pub flags: u16,
    pub position: u8,
    pub group: u8,
    pub group_mask: u8,
    /// Currently mapped control ID (equal to `cid` when not remapped).
    pub mapped_cid: u16,
}

impl LogitechControlInfo {
    pub const fn is_reprogrammable(&self) -> bool {
        self.flags & KEY_FLAG_REPROGRAMMABLE != 0
    }

    pub const fn is_remapped(&self) -> bool {
        self.mapped_cid != self.cid
    }

    /// Returns `true` when `target` is `0x0000` (restore default), this control itself, or a
    /// control whose group bit (`1 << (group - 1)`) is set in this control's `group_mask`.
    pub fn is_legal_target(&self, target: u16, controls: &[Self]) -> bool {
        if !self.is_reprogrammable() || self.group_mask == 0 {
            return false;
        }
        if target == 0x0000 || target == self.cid {
            return true;
        }
        controls.iter().any(|c| {
            c.cid == target
                && (1..=8).contains(&c.group)
                && self.group_mask & (1 << (c.group - 1)) != 0
        })
    }
}

impl<T: HidTransport> LogitechHidppDevice<T> {
    pub(crate) fn discover_controls(&mut self) -> CoreResult<Vec<LogitechControlInfo>> {
        if self.cached_feature(feature::REPROG_CONTROLS_V4).is_none() {
            return Ok(Vec::new());
        }
        let count = self.call_feature(feature::REPROG_CONTROLS_V4, 0, &[], false)?[0];
        let mut controls = Vec::with_capacity(usize::from(count));
        for pos in 0..count {
            let info = self.call_feature(feature::REPROG_CONTROLS_V4, 1, &[pos], false)?;
            let cid = u16::from_be_bytes([info[0], info[1]]);
            let mapped_cid = self.read_mapped_cid(cid)?;
            controls.push(LogitechControlInfo {
                cid,
                task_id: u16::from_be_bytes([info[2], info[3]]),
                flags: u16::from_be_bytes([info[8], info[4]]),
                position: info[5],
                group: info[6],
                group_mask: info[7],
                mapped_cid,
            });
        }
        Ok(controls)
    }

    fn read_mapped_cid(&mut self, cid: u16) -> CoreResult<u16> {
        let resp = self.call_feature(feature::REPROG_CONTROLS_V4, 2, &cid.to_be_bytes(), false)?;
        let mapped = u16::from_be_bytes([resp[3], resp[4]]);
        Ok(if mapped == 0 { cid } else { mapped })
    }

    /// Re-reads every `ReprogControlsV4` control (count, info and current mapping).
    pub fn read_reprog_controls(&mut self) -> CoreResult<Vec<LogitechControlInfo>> {
        if self.cached_feature(feature::REPROG_CONTROLS_V4).is_none() {
            return Err(CoreError::UnsupportedCapability("reprog_controls_v4"));
        }
        self.controls = self.discover_controls()?;
        Ok(self.controls.clone())
    }

    /// Remaps control `cid` to `target_cid` (`0x0000` restores the default mapping).
    pub fn remap_control(&mut self, cid: u16, target_cid: u16) -> CoreResult<()> {
        let control = self
            .controls
            .iter()
            .find(|c| c.cid == cid)
            .copied()
            .ok_or_else(|| CoreError::Validation(format!("unknown control 0x{cid:04X}")))?;
        if !control.is_reprogrammable() || control.group_mask == 0 {
            return Err(CoreError::Validation(format!(
                "control 0x{cid:04X} is not reprogrammable"
            )));
        }
        if !control.is_legal_target(target_cid, &self.controls) {
            return Err(CoreError::Validation(format!(
                "control 0x{target_cid:04X} is not a legal remap target for 0x{cid:04X} (group mask 0x{:02X})",
                control.group_mask
            )));
        }

        let [cid_hi, cid_lo] = cid.to_be_bytes();
        let [target_hi, target_lo] = target_cid.to_be_bytes();
        self.call_feature(
            feature::REPROG_CONTROLS_V4,
            3,
            &[cid_hi, cid_lo, 0x00, target_hi, target_lo],
            true,
        )?;

        if let Some(entry) = self.controls.iter_mut().find(|c| c.cid == cid) {
            entry.mapped_cid = if target_cid == 0 { cid } else { target_cid };
        }
        Ok(())
    }
}

fn cid_to_key_code(cid: u16) -> KeyCode {
    let [hi, lo] = cid.to_be_bytes();
    KeyCode::new(hi, lo)
}

impl<T: HidTransport> KeymapController for LogitechHidppDevice<T> {
    fn keymap_capabilities(&self) -> KeymapCapabilities {
        let cols = u8::try_from(self.controls.len()).unwrap_or(u8::MAX);
        KeymapCapabilities::new(vec!["Controls".to_owned()], 1, cols, true)
    }

    fn read_keymap(&mut self) -> CoreResult<KeymapSnapshot> {
        let cids: Vec<u16> = self.controls.iter().map(|c| c.cid).collect();
        let mut layer = Vec::with_capacity(cids.len());
        for (ix, cid) in cids.into_iter().enumerate() {
            let mapped = self.read_mapped_cid(cid)?;
            self.controls[ix].mapped_cid = mapped;
            layer.push(cid_to_key_code(mapped));
        }
        Ok(KeymapSnapshot::new(vec![layer], 0))
    }

    fn set_key_binding(&mut self, layer: u8, key: KeyIndex, code: KeyCode) -> CoreResult<()> {
        self.keymap_capabilities().validate_layer(layer)?;
        let cid = self
            .controls
            .get(usize::from(key.value()))
            .map(|c| c.cid)
            .ok_or_else(|| CoreError::OutOfRange {
                field: "key_index",
                actual: key.value().to_string(),
                expected: format!("0..{}", self.controls.len()),
            })?;
        let (hi, lo) = code.as_tuple();
        self.remap_control(cid, u16::from_be_bytes([hi, lo]))
    }

    fn reset_keymap(&mut self) -> CoreResult<()> {
        let remapped: Vec<u16> = self
            .controls
            .iter()
            .filter(|c| c.is_reprogrammable() && c.group_mask != 0 && c.is_remapped())
            .map(|c| c.cid)
            .collect();
        for cid in remapped {
            self.remap_control(cid, 0x0000)?;
        }
        Ok(())
    }
}
