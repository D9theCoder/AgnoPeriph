//! `ColorLEDEffects` (`0x8070`) and `RGBEffects` (`0x8071`) lighting support.

use agnoperiph_core::{
    Brightness, CoreError, CoreResult, RgbCapabilities, RgbController, RgbEffect, RgbEffectKind,
    RgbSnapshot, RgbZoneId, RgbZoneInfo, RgbZoneState,
};
use agnoperiph_transport::HidTransport;

use crate::LogitechHidppDevice;
use crate::protocol::{
    decode_led_effect_params, encode_led_effect_params, feature, full_brightness, led_effect,
    led_effect_candidates, led_effect_kind, led_zone_name,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LedFeature {
    ColorLedEffects { has_readback: bool },
    RgbEffects,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LedZone {
    pub(crate) index: u8,
    pub(crate) location: u16,
    /// Effect IDs indexed by effect ordinal.
    pub(crate) effect_ids: Vec<u16>,
}

impl LedZone {
    fn ordinal_for(&self, kind: RgbEffectKind) -> Option<(u8, u16)> {
        led_effect_candidates(kind).iter().find_map(|&wanted| {
            self.effect_ids
                .iter()
                .position(|&id| id == wanted)
                .and_then(|ord| u8::try_from(ord).ok())
                .map(|ord| (ord, wanted))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LedDescriptor {
    pub(crate) feature: LedFeature,
    pub(crate) zones: Vec<LedZone>,
}

impl<T: HidTransport> LogitechHidppDevice<T> {
    /// Reads zone and effect tables, preferring `0x8071` over `0x8070`.
    pub(crate) fn discover_led_effects(&mut self) -> CoreResult<Option<LedDescriptor>> {
        if self.cached_feature(feature::RGB_EFFECTS).is_some() {
            let info = self.call_feature(feature::RGB_EFFECTS, 0, &[0xFF, 0xFF], false)?;
            let mut zones = Vec::with_capacity(usize::from(info[2]));
            for cluster in 0..info[2] {
                let zone_info =
                    self.call_feature(feature::RGB_EFFECTS, 0, &[cluster, 0xFF], false)?;
                let location = u16::from_be_bytes([zone_info[2], zone_info[3]]);
                let mut effect_ids = Vec::with_capacity(usize::from(zone_info[4]));
                for ord in 0..zone_info[4] {
                    let effect =
                        self.call_feature(feature::RGB_EFFECTS, 0, &[cluster, ord], false)?;
                    effect_ids.push(u16::from_be_bytes([effect[2], effect[3]]));
                }
                zones.push(LedZone {
                    index: cluster,
                    location,
                    effect_ids,
                });
            }
            return Ok(Some(LedDescriptor {
                feature: LedFeature::RgbEffects,
                zones,
            }));
        }

        if self.cached_feature(feature::COLOR_LED_EFFECTS).is_some() {
            let info = self.call_feature(feature::COLOR_LED_EFFECTS, 0, &[], false)?;
            let has_readback = u16::from_be_bytes([info[3], info[4]]) & 0x0001 != 0;
            let mut zones = Vec::with_capacity(usize::from(info[0]));
            for zone in 0..info[0] {
                let zone_info = self.call_feature(feature::COLOR_LED_EFFECTS, 1, &[zone], false)?;
                let location = u16::from_be_bytes([zone_info[1], zone_info[2]]);
                let mut effect_ids = Vec::with_capacity(usize::from(zone_info[3]));
                for ord in 0..zone_info[3] {
                    let effect =
                        self.call_feature(feature::COLOR_LED_EFFECTS, 2, &[zone, ord], false)?;
                    effect_ids.push(u16::from_be_bytes([effect[2], effect[3]]));
                }
                zones.push(LedZone {
                    index: zone,
                    location,
                    effect_ids,
                });
            }
            return Ok(Some(LedDescriptor {
                feature: LedFeature::ColorLedEffects { has_readback },
                zones,
            }));
        }

        Ok(None)
    }

    fn led_zone(&self, zone: RgbZoneId) -> CoreResult<(LedFeature, LedZone)> {
        let led = self
            .led
            .as_ref()
            .ok_or(CoreError::UnsupportedCapability("rgb"))?;
        let found = led
            .zones
            .iter()
            .find(|z| z.index == zone.value())
            .cloned()
            .ok_or_else(|| CoreError::OutOfRange {
                field: "rgb_zone",
                actual: zone.value().to_string(),
                expected: format!("0..{}", led.zones.len()),
            })?;
        Ok((led.feature, found))
    }

    fn fallback_zone_state(&self, zone: u8) -> RgbZoneState {
        self.rgb_cache.get(&zone).copied().unwrap_or_else(|| {
            RgbZoneState::new(RgbZoneId::new(zone), RgbEffect::Off, full_brightness())
        })
    }
}

impl<T: HidTransport> RgbController for LogitechHidppDevice<T> {
    fn rgb_capabilities(&self) -> RgbCapabilities {
        let Some(led) = &self.led else {
            return RgbCapabilities::new(Vec::new(), Vec::new(), false);
        };
        let zones = led
            .zones
            .iter()
            .map(|z| RgbZoneInfo::new(RgbZoneId::new(z.index), led_zone_name(z.location, z.index)))
            .collect();
        let mut kinds: Vec<RgbEffectKind> = Vec::new();
        for kind in led
            .zones
            .iter()
            .flat_map(|z| z.effect_ids.iter().filter_map(|&id| led_effect_kind(id)))
        {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        RgbCapabilities::new(zones, kinds, false)
    }

    fn read_rgb_state(&mut self) -> CoreResult<RgbSnapshot> {
        let Some(led) = self.led.clone() else {
            return Err(CoreError::UnsupportedCapability("rgb"));
        };
        let mut states = Vec::with_capacity(led.zones.len());
        for zone in &led.zones {
            let decoded = if led.feature == (LedFeature::ColorLedEffects { has_readback: true }) {
                let resp =
                    self.call_feature(feature::COLOR_LED_EFFECTS, 14, &[zone.index], false)?;
                zone.effect_ids
                    .get(usize::from(resp[1]))
                    .and_then(|&id| decode_led_effect_params(id, &resp[2..12]))
                    .map(|(effect, brightness)| {
                        RgbZoneState::new(RgbZoneId::new(zone.index), effect, brightness)
                    })
            } else {
                None
            };
            states.push(decoded.unwrap_or_else(|| self.fallback_zone_state(zone.index)));
        }
        Ok(RgbSnapshot::new(states))
    }

    fn set_zone_effect(
        &mut self,
        zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    ) -> CoreResult<()> {
        let (led_feature, led_zone) = self.led_zone(zone)?;
        let (ord, effect_id) = led_zone.ordinal_for(effect.kind()).ok_or_else(|| {
            CoreError::Validation(format!(
                "zone {} does not support effect {:?}",
                zone.value(),
                effect.kind()
            ))
        })?;
        let effect_params = encode_led_effect_params(effect_id, &effect, brightness)?;

        let mut request = [0u8; 16];
        request[0] = led_zone.index;
        request[1] = ord;
        request[2..12].copy_from_slice(&effect_params);
        match led_feature {
            LedFeature::RgbEffects => {
                // Re-claimed on every set: the claim is lost when the keyboard sleeps or
                // power-cycles behind a receiver.
                self.call_feature(feature::RGB_EFFECTS, 5, &[0x01, 0x01], true)?;
                request[12] = 0x01;
                self.call_feature(feature::RGB_EFFECTS, 1, &request, true)?;
            }
            LedFeature::ColorLedEffects { .. } => {
                // The documented 0x8070 Static layout is `[r, g, b]` only; the `0x02`
                // no-ramp byte is the 0x8071 cluster form.
                if effect_id == led_effect::STATIC {
                    request[5] = 0x00;
                }
                self.call_feature(feature::COLOR_LED_EFFECTS, 3, &request, true)?;
            }
        }

        self.rgb_cache
            .insert(led_zone.index, RgbZoneState::new(zone, effect, brightness));
        Ok(())
    }
}
