//! Profile JSON export and import, backed by [`ExportedProfile`].
//!
//! Import never writes directly: a parsed file is validated against the selected device's
//! capabilities, previewed section by section, and its supported sections are staged as
//! unsaved edits.

use agnoperiph_core::{
    ExportedHallEffectProfile, ExportedProfile, KeyboardConfigSnapshot, TravelHundredthsMm,
};
use std::fmt::Write as _;

use crate::app::mm;
use crate::backend::DeviceSnapshot;
use crate::draft::{DeviceDraft, ZoneDraft};
use crate::hall_effect_panel::snap_travel;

/// Builds an export of `device`'s values (already carrying any unsaved edits).
pub fn export_profile(device: &DeviceSnapshot, name: &str) -> ExportedProfile {
    let mut profile =
        ExportedProfile::new(name).with_device_hint(device.info.vendor, &device.info.model_name);
    if let Some(pointer) = &device.pointer {
        profile = profile.with_pointer(pointer.snapshot.clone());
    }
    if let Some(he) = &device.hall_effect {
        let mut section = ExportedHallEffectProfile::default();
        section.keys.clone_from(&he.snapshot.keys);
        if let Some(first) = he.snapshot.keys.first() {
            section.top_deadzone = first.top_deadzone;
            section.bottom_deadzone = first.bottom_deadzone;
        }
        section.socd_pairs.clone_from(&he.snapshot.socd_pairs);
        section
            .rapid_switch_pairs
            .clone_from(&he.snapshot.rapid_switch_pairs);
        section.dks_bindings.clone_from(&he.snapshot.dks_bindings);
        section
            .mod_tap_bindings
            .clone_from(&he.snapshot.mod_tap_bindings);
        section
            .toggle_bindings
            .clone_from(&he.snapshot.toggle_bindings);
        profile = profile.with_hall_effect(section);
    }
    if let Some(rgb) = &device.rgb {
        profile = profile.with_rgb_zones(rgb.snapshot.zones.clone());
    }
    if let Some(kc) = &device.keyboard_config {
        profile = profile.with_keyboard_config(kc.snapshot);
    }
    profile
}

/// Serializes an export as pretty JSON.
pub fn export_json(device: &DeviceSnapshot, name: &str) -> String {
    serde_json::to_string_pretty(&export_profile(device, name)).unwrap_or_default()
}

/// Suggested file name, e.g. `ATK RS7 - Default.json`.
pub fn export_file_name(device: &DeviceSnapshot, preset: Option<&str>) -> String {
    let raw = match preset {
        Some(preset) => format!("{} - {preset}", device.info.model_name),
        None => device.info.model_name.clone(),
    };
    let safe: String = raw
        .chars()
        .map(|c| if r#"<>:"/\|?*"#.contains(c) { '-' } else { c })
        .collect();
    format!("{safe}.json")
}

/// One section of an imported file and whether it loads on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSection {
    pub name: &'static str,
    pub detail: String,
    pub loads: bool,
}

/// Parsed and validated import, as the preview dialog shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportPreview {
    pub file_name: String,
    pub profile: Option<ExportedProfile>,
    pub made_for: Option<String>,
    pub matches_model: bool,
    pub sections: Vec<ImportSection>,
    pub error: Option<String>,
}

impl ImportPreview {
    /// `true` when at least one section can be staged.
    pub fn can_import(&self) -> bool {
        self.error.is_none() && self.sections.iter().any(|s| s.loads)
    }

    pub fn parse(file_name: &str, text: &str, device: &DeviceSnapshot) -> Self {
        let mut preview = Self {
            file_name: file_name.to_owned(),
            profile: None,
            made_for: None,
            matches_model: true,
            sections: Vec::new(),
            error: None,
        };
        let profile: ExportedProfile = match serde_json::from_str(text) {
            Ok(profile) => profile,
            Err(err) => {
                preview.error = Some(format!("This file isn’t an AgnoPeriph profile ({err})."));
                return preview;
            }
        };
        preview.made_for.clone_from(&profile.model_name);
        preview.matches_model = profile
            .model_name
            .as_deref()
            .is_none_or(|m| m == device.info.model_name);

        if let Some(pointer) = &profile.pointer {
            let stages = pointer
                .dpi_stages
                .iter()
                .map(|s| s.x.value().to_string())
                .collect::<Vec<_>>()
                .join(" / ");
            preview.sections.push(ImportSection {
                name: "Sensor",
                detail: format!("{stages} DPI, {}", pointer.polling_rate),
                loads: device.pointer.is_some(),
            });
        }
        if let Some(he) = &profile.hall_effect {
            let rt = he
                .keys
                .iter()
                .filter(|k| k.rapid_trigger.is_enabled())
                .count();
            let mut detail = format!("Actuation on {} keys, Rapid Trigger on {rt}", he.keys.len());
            if let (Some(top), Some(bottom)) = (he.top_deadzone, he.bottom_deadzone) {
                let _ = write!(detail, ", dead zones {} / {} mm", mm(top), mm(bottom));
            }
            preview.sections.push(ImportSection {
                name: "Triggers",
                detail,
                loads: device.hall_effect.is_some(),
            });
            let paired = he.socd_pairs.len()
                + he.rapid_switch_pairs.len()
                + he.dks_bindings.len()
                + he.mod_tap_bindings.len()
                + he.toggle_bindings.len();
            if paired > 0 {
                preview.sections.push(ImportSection {
                    name: "Paired keys",
                    detail: format!("{paired} bindings, kept as they are on the keyboard"),
                    loads: false,
                });
            }
        }
        if !profile.rgb_zones.is_empty() {
            let supported = device.rgb.as_ref().is_some_and(|rgb| {
                profile
                    .rgb_zones
                    .iter()
                    .any(|z| rgb.capabilities.zones.iter().any(|info| info.id == z.zone))
            });
            preview.sections.push(ImportSection {
                name: "Lighting",
                detail: format!("{} zones", profile.rgb_zones.len()),
                loads: supported,
            });
        }
        if profile.keyboard_config.is_some() {
            preview.sections.push(ImportSection {
                name: "Keyboard settings",
                detail: "Layout, key conflict and polling options".to_owned(),
                loads: device.keyboard_config.is_some(),
            });
        }

        let validated = profile.validate(
            device.pointer.as_ref().map(|p| &p.capabilities),
            device.hall_effect.as_ref().map(|h| &h.capabilities),
            device.rgb.as_ref().map(|r| &r.capabilities),
            device.keyboard_config.as_ref().map(|k| &k.capabilities),
        );
        preview.error = match validated {
            Err(agnoperiph_core::CoreError::UnsupportedCapability(_)) => Some(format!(
                "{} doesn’t support the settings in this file.",
                device.info.model_name
            )),
            Err(err) => Some(format!(
                "This profile doesn’t fit {}: {err}.",
                device.info.model_name
            )),
            Ok(()) if !preview.sections.iter().any(|s| s.loads) => Some(format!(
                "Nothing in this file applies to {}.",
                device.info.model_name
            )),
            Ok(()) => None,
        };
        preview.profile = Some(profile);
        preview
    }
}

/// Stages every supported section of `profile` onto `draft` for `device` (raw values).
pub(crate) fn stage_import(
    profile: &ExportedProfile,
    device: &DeviceSnapshot,
    draft: &mut DeviceDraft,
) {
    if let (Some(pointer), Some(state)) = (&profile.pointer, &device.pointer) {
        let caps = &state.capabilities;
        draft.pointer.dpi_stages = Some(pointer.dpi_stages.clone());
        if caps.supported_polling_rates.contains(&pointer.polling_rate) {
            draft.pointer.polling_rate = Some(pointer.polling_rate);
        }
        draft.pointer.lod = pointer.lod.filter(|l| caps.supported_lods.contains(l));
        draft.pointer.debounce = pointer
            .debounce_ms
            .filter(|_| caps.debounce_range_ms.is_some());
        draft.pointer.motion_sync = pointer.motion_sync.filter(|_| caps.supports_motion_sync);
        draft.pointer.angle_snapping = pointer
            .angle_snapping
            .filter(|_| caps.supports_angle_snapping);
        draft.pointer.ripple_control = pointer
            .ripple_control
            .filter(|_| caps.supports_ripple_control);
    }
    if let (Some(he), Some(state)) = (&profile.hall_effect, &device.hall_effect) {
        let keys = &state.snapshot.keys;
        for config in &he.keys {
            if keys.iter().any(|k| k.key == config.key) {
                draft.actuation.insert(config.key, config.actuation);
                if state.capabilities.has_rapid_trigger() {
                    draft.rapid_trigger.insert(config.key, config.rapid_trigger);
                }
            }
        }
        if he.keys.is_empty()
            && let Some(point) = he.global_actuation
        {
            for key in keys {
                draft.actuation.insert(key.key, point);
            }
        }
        if let (Some(top), Some(bottom)) = (he.top_deadzone, he.bottom_deadzone) {
            let clamp = |v: TravelHundredthsMm| {
                snap_travel(
                    i32::from(v.as_hundredths()),
                    TravelHundredthsMm::from_hundredths(0),
                    TravelHundredthsMm::from_hundredths(100),
                    TravelHundredthsMm::from_hundredths(1),
                )
            };
            draft.deadzones = Some((clamp(top), clamp(bottom)));
        }
    }
    if let Some(rgb) = &device.rgb {
        for zone in &profile.rgb_zones {
            if rgb
                .capabilities
                .zones
                .iter()
                .any(|info| info.id == zone.zone)
            {
                draft.rgb.insert(
                    zone.zone.value(),
                    ZoneDraft {
                        effect: zone.effect,
                        brightness: zone.brightness,
                    },
                );
            }
        }
    }
    if let (Some(cfg), Some(state)) = (profile.keyboard_config, &device.keyboard_config) {
        draft.keyboard_config = Some(merge_config(cfg, state.snapshot, &state.capabilities));
    }
    draft.normalize(device);
}

/// Takes `incoming` values only for options the keyboard supports.
fn merge_config(
    incoming: KeyboardConfigSnapshot,
    current: KeyboardConfigSnapshot,
    caps: &agnoperiph_core::KeyboardConfigCapabilities,
) -> KeyboardConfigSnapshot {
    let mut out = current;
    let pick = |supported: bool, new: bool, old: bool| if supported { new } else { old };
    out.mac_mode = pick(caps.supports_mac_mode, incoming.mac_mode, current.mac_mode);
    out.exchange_wasd = pick(
        caps.supports_wasd_swap,
        incoming.exchange_wasd,
        current.exchange_wasd,
    );
    out.win_lock = pick(caps.supports_win_lock, incoming.win_lock, current.win_lock);
    out.six_key_rollover = pick(
        caps.supports_six_key_rollover,
        incoming.six_key_rollover,
        current.six_key_rollover,
    );
    out.anti_accidental_touch = pick(
        caps.supports_anti_accidental_touch,
        incoming.anti_accidental_touch,
        current.anti_accidental_touch,
    );
    out.extreme_mode = pick(
        caps.supports_extreme_mode,
        incoming.extreme_mode,
        current.extreme_mode,
    );
    out.fn_inversion = pick(
        caps.supports_fn_inversion,
        incoming.fn_inversion,
        current.fn_inversion,
    );
    out.game_mode = pick(
        caps.supports_game_mode,
        incoming.game_mode,
        current.game_mode,
    );
    if caps
        .supported_polling_rates
        .contains(&incoming.polling_rate)
    {
        out.polling_rate = incoming.polling_rate;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        MOCK_KEYBOARD_ID, MOCK_MOUSE_ID, MockPeripheralService, PeripheralService,
    };
    use agnoperiph_core::DeviceId;

    fn device(id: &str) -> DeviceSnapshot {
        MockPeripheralService::new()
            .refresh()
            .unwrap()
            .into_iter()
            .find(|d| d.id() == &DeviceId::new(id))
            .unwrap()
    }

    #[test]
    fn export_round_trips_into_an_importable_preview() {
        let kb = device(MOCK_KEYBOARD_ID);
        let json = export_json(&kb, "Scrim night");
        let preview = ImportPreview::parse("scrim.json", &json, &kb);
        assert!(preview.can_import(), "{:?}", preview.error);
        assert!(preview.matches_model);
        assert!(
            preview
                .sections
                .iter()
                .any(|s| s.name == "Triggers" && s.loads)
        );

        // Importing the device's own values stages nothing.
        let mut draft = DeviceDraft::default();
        stage_import(preview.profile.as_ref().unwrap(), &kb, &mut draft);
        assert!(draft.is_empty());
    }

    #[test]
    fn mouse_profile_is_refused_on_a_keyboard() {
        let kb = device(MOCK_KEYBOARD_ID);
        let mouse = device(MOCK_MOUSE_ID);
        let json = export_json(&mouse, "Atlantis FPS");
        let preview = ImportPreview::parse("atlantis-fps.json", &json, &kb);
        assert!(!preview.can_import());
        assert!(!preview.matches_model);
        assert!(preview.error.unwrap().contains("doesn’t support"));
    }

    #[test]
    fn invalid_json_and_wrong_schema_are_reported() {
        let kb = device(MOCK_KEYBOARD_ID);
        let preview = ImportPreview::parse("notes.json", "{ not json", &kb);
        assert!(
            preview
                .error
                .unwrap()
                .contains("isn’t an AgnoPeriph profile")
        );

        let mut value: serde_json::Value = serde_json::from_str(&export_json(&kb, "Old")).unwrap();
        value["schema_version"] = serde_json::json!(99);
        let preview = ImportPreview::parse("old.json", &value.to_string(), &kb);
        assert!(preview.error.unwrap().contains("schema_version"));
    }

    #[test]
    fn edited_values_stage_as_unsaved_changes() {
        let kb = device(MOCK_KEYBOARD_ID);
        let mut value: serde_json::Value =
            serde_json::from_str(&export_json(&kb, "Edited")).unwrap();
        value["hall_effect"]["keys"][0]["actuation"] = serde_json::json!(80);
        let preview = ImportPreview::parse("edited.json", &value.to_string(), &kb);
        assert!(preview.can_import(), "{:?}", preview.error);
        let mut draft = DeviceDraft::default();
        stage_import(preview.profile.as_ref().unwrap(), &kb, &mut draft);
        assert_eq!(draft.actuation.len(), 1);
    }

    #[test]
    fn file_names_drop_reserved_characters() {
        let kb = device(MOCK_KEYBOARD_ID);
        let name = export_file_name(&kb, Some("FPS/Gaming"));
        assert!(name.ends_with("FPS-Gaming.json"));
    }
}
