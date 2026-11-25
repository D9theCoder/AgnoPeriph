use agnoperiph_core::{
    Brightness, KeyCode, KeyIndex, Peripheral, PollingRateHz, RgbColor, RgbDirection, RgbEffect,
    RgbEffectKind, RgbZoneId,
};
use agnoperiph_transport::{HidDeviceDescriptor, MockHidTransport};

use crate::LogitechHidppDevice;
use crate::protocol::{HIDPP_USAGE_PAGE_USB, LOGITECH_VID, feature};
use crate::tests::make_reply;

const DEV: u8 = 0xFF;
const IDX_REPORT_RATE: u8 = 0x09;

const fn index_of(feature_id: u16) -> u8 {
    match feature_id {
        feature::RGB_EFFECTS => 0x0A,
        feature::COLOR_LED_EFFECTS => 0x0B,
        feature::K375S_FN_INVERSION => 0x0C,
        feature::NEW_FN_INVERSION => 0x0D,
        feature::FN_INVERSION => 0x0E,
        feature::DISABLE_KEYS_BY_USAGE => 0x0F,
        feature::REPROG_CONTROLS_V4 => 0x10,
        _ => 0x00,
    }
}

fn reply(feat_idx: u8, func: u8, params: &[u8]) -> Vec<u8> {
    make_reply(DEV, feat_idx, func, params)
}

/// Wired keyboard answering every mouse-side probe as absent, then the keyboard probes
/// in `feature::KEYBOARD_PROBE_ORDER` with the indices from [`index_of`] for `present`.
fn keyboard_mock(rate_mask: Option<u8>, present: &[u16]) -> MockHidTransport {
    let desc = HidDeviceDescriptor::new(
        LOGITECH_VID,
        0xC33F,
        HIDPP_USAGE_PAGE_USB,
        0x0001,
        1,
        "mock://logitech/keyboard",
    );
    let mut mock = MockHidTransport::new(desc);
    mock.push_input_response(reply(0x00, 1, &[0x04, 0x02, 0x5A]));
    // 0x0005, 0x0003, 0x1004, 0x1000, 0x1001, 0x2201 absent
    for _ in 0..6 {
        mock.push_input_response(reply(0x00, 0, &[0x00]));
    }
    match rate_mask {
        Some(mask) => {
            mock.push_input_response(reply(0x00, 0, &[IDX_REPORT_RATE]));
            mock.push_input_response(reply(IDX_REPORT_RATE, 0, &[mask]));
        }
        None => mock.push_input_response(reply(0x00, 0, &[0x00])),
    }
    // 0x8100 absent
    mock.push_input_response(reply(0x00, 0, &[0x00]));
    for feature_id in feature::KEYBOARD_PROBE_ORDER {
        let idx = if present.contains(&feature_id) {
            index_of(feature_id)
        } else {
            0x00
        };
        mock.push_input_response(reply(0x00, 0, &[idx]));
    }
    mock
}

fn open(mock: MockHidTransport) -> LogitechHidppDevice<MockHidTransport> {
    LogitechHidppDevice::open(mock, None, false, None).unwrap()
}

fn writes_to(dev: &LogitechHidppDevice<MockHidTransport>, feat_idx: u8) -> Vec<Vec<u8>> {
    dev.transport()
        .inner()
        .recorded_writes()
        .iter()
        .filter(|w| w.bytes[2] == feat_idx)
        .map(|w| w.bytes.clone())
        .collect()
}

fn write_count(dev: &LogitechHidppDevice<MockHidTransport>) -> usize {
    dev.transport().inner().recorded_writes().len()
}

fn frame(feat_idx: u8, func: u8, params: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 20];
    f[0] = 0x11;
    f[1] = DEV;
    f[2] = feat_idx;
    f[3] = (func << 4) | 0x0C;
    f[4..4 + params.len()].copy_from_slice(params);
    f
}

fn pct(value: u8) -> Brightness {
    Brightness::new(value).unwrap()
}

#[test]
fn rgb_effects_8071_claims_software_control_then_sets_cluster_effects() {
    let idx = index_of(feature::RGB_EFFECTS);
    let mut mock = keyboard_mock(None, &[feature::RGB_EFFECTS]);
    mock.push_input_response(reply(idx, 0, &[0xFF, 0xFF, 0x01]));
    mock.push_input_response(reply(idx, 0, &[0x00, 0xFF, 0x00, 0x01, 0x04]));
    // Offers both Wave IDs: 0x16 must win over the unverified 0x04 layout.
    for (ord, id) in [(0u8, 0x00u8), (1, 0x01), (2, 0x04), (3, 0x16)] {
        mock.push_input_response(reply(idx, 0, &[0x00, ord, 0x00, id]));
    }
    for _ in 0..2 {
        mock.push_input_response(reply(idx, 5, &[]));
        mock.push_input_response(reply(idx, 1, &[]));
    }

    let mut dev = open(mock);
    assert!(dev.capabilities().has_rgb());
    assert!(!dev.capabilities().has_keyboard_config());
    assert!(!dev.capabilities().has_keymap());

    let rgb = dev.as_rgb_mut().unwrap();
    let caps = rgb.rgb_capabilities();
    assert_eq!(caps.zones.len(), 1);
    assert_eq!(caps.zones[0].name, "Primary");
    assert_eq!(
        caps.supported_effects,
        vec![
            RgbEffectKind::Off,
            RgbEffectKind::Static,
            RgbEffectKind::Wave
        ]
    );

    let zone = RgbZoneId::new(0);
    let red = RgbEffect::Static {
        color: RgbColor::new(0xFF, 0x00, 0x00),
    };
    rgb.set_zone_effect(zone, red, pct(50)).unwrap();
    let wave = RgbEffect::Wave {
        speed: 255,
        direction: RgbDirection::LeftToRight,
    };
    rgb.set_zone_effect(zone, wave, pct(80)).unwrap();

    let breathing = RgbEffect::Breathing {
        color: RgbColor::WHITE,
        speed: 10,
    };
    let before = write_count(&dev);
    assert!(
        dev.as_rgb_mut()
            .unwrap()
            .set_zone_effect(zone, breathing, pct(50))
            .is_err()
    );
    assert_eq!(write_count(&dev), before);

    let writes = writes_to(&dev, idx);
    let sets: Vec<_> = writes.iter().filter(|w| w[3] != 0x0C).collect();
    let claim = frame(idx, 5, &[0x01, 0x01]);
    assert_eq!(
        sets,
        vec![
            &claim,
            &frame(
                idx,
                1,
                &[0x00, 0x01, 0x7F, 0x00, 0x00, 0x02, 0, 0, 0, 0, 0, 0, 0x01]
            ),
            &claim,
            // Wave 0x16 (ord 3): saturation p1, period 1000 ms at p6..7, intensity p8, direction p9
            &frame(
                idx,
                1,
                &[0x00, 0x03, 0, 0xFF, 0, 0, 0, 0, 0x03, 0xE8, 80, 0x01, 0x01]
            ),
        ]
    );

    let snap = dev.as_rgb_mut().unwrap().read_rgb_state().unwrap();
    assert_eq!(snap.zones[0].effect, wave);
    assert_eq!(snap.zones[0].brightness, pct(80));
}

#[test]
fn color_led_effects_8070_writes_ram_effect_and_reads_it_back() {
    let idx = index_of(feature::COLOR_LED_EFFECTS);
    let mut mock = keyboard_mock(None, &[feature::COLOR_LED_EFFECTS]);
    mock.push_input_response(reply(idx, 0, &[0x01, 0x00, 0x00, 0x00, 0x01]));
    mock.push_input_response(reply(idx, 1, &[0x00, 0x00, 0x02, 0x03]));
    for (ord, id) in [(0u8, 0x01u8), (1, 0x0A), (2, 0x03)] {
        mock.push_input_response(reply(idx, 2, &[0x00, ord, 0x00, id]));
    }
    mock.push_input_response(reply(idx, 3, &[]));
    mock.push_input_response(reply(idx, 3, &[]));
    let breathing_params = [0x00, 0x80, 0xFF, 0x0C, 0x80, 0x00, 60, 0, 0, 0];
    let mut readback = vec![0x00, 0x01];
    readback.extend_from_slice(&breathing_params);
    mock.push_input_response(reply(idx, 14, &readback));

    let mut dev = open(mock);
    assert!(dev.capabilities().has_rgb());
    let rgb = dev.as_rgb_mut().unwrap();
    let caps = rgb.rgb_capabilities();
    assert_eq!(caps.zones[0].name, "Logo");
    assert_eq!(
        caps.supported_effects,
        vec![
            RgbEffectKind::Static,
            RgbEffectKind::Breathing,
            RgbEffectKind::SpectrumCycle
        ]
    );

    let zone = RgbZoneId::new(0);
    let breathing = RgbEffect::Breathing {
        color: RgbColor::new(0x00, 0x80, 0xFF),
        speed: 200,
    };
    rgb.set_zone_effect(zone, breathing, pct(60)).unwrap();
    rgb.set_zone_effect(
        zone,
        RgbEffect::Static {
            color: RgbColor::WHITE,
        },
        pct(100),
    )
    .unwrap();

    let snap = rgb.read_rgb_state().unwrap();
    assert_eq!(snap.zones[0].effect, breathing);
    assert_eq!(snap.zones[0].brightness, pct(60));

    let writes = writes_to(&dev, idx);
    let sets: Vec<_> = writes.iter().filter(|w| w[3] == 0x3C).collect();
    let mut expected = vec![0x00, 0x01];
    expected.extend_from_slice(&breathing_params);
    assert_eq!(*sets[0], frame(idx, 3, &expected));
    assert_eq!(
        *sets[1],
        frame(idx, 3, &[0x00, 0x00, 0xFF, 0xFF, 0xFF, 0x00])
    );
    assert!(sets.iter().all(|w| w[16..].iter().all(|&b| b == 0)));
}

#[test]
fn k375s_fn_inversion_takes_precedence_and_targets_current_host() {
    let idx = index_of(feature::K375S_FN_INVERSION);
    let mut mock = keyboard_mock(
        None,
        &[
            feature::K375S_FN_INVERSION,
            feature::NEW_FN_INVERSION,
            feature::FN_INVERSION,
        ],
    );
    mock.push_input_response(reply(idx, 0, &[0x00, 0x00, 0x00, 0x00]));
    mock.push_input_response(reply(idx, 0, &[0x00, 0x00, 0x00, 0x00]));
    mock.push_input_response(reply(idx, 1, &[0x00, 0x01]));

    let mut dev = open(mock);
    assert!(dev.capabilities().has_keyboard_config());
    let cfg = dev.as_keyboard_config_mut().unwrap();
    let caps = cfg.keyboard_config_capabilities();
    assert!(caps.supports_fn_inversion);
    assert!(!caps.supports_game_mode);
    assert!(!caps.supports_win_lock);

    let current = cfg.read_keyboard_config().unwrap();
    assert!(!current.fn_inversion);
    cfg.set_keyboard_config(&current.with_fn_inversion(true))
        .unwrap();

    let writes = writes_to(&dev, idx);
    assert_eq!(writes[0], frame(idx, 0, &[0xFF]));
    assert_eq!(writes.last().unwrap(), &frame(idx, 1, &[0xFF, 0x01]));
}

#[test]
fn new_fn_inversion_reads_and_clears_state() {
    let idx = index_of(feature::NEW_FN_INVERSION);
    let mut mock = keyboard_mock(None, &[feature::NEW_FN_INVERSION]);
    mock.push_input_response(reply(idx, 0, &[0x01, 0x00]));
    mock.push_input_response(reply(idx, 0, &[0x01, 0x00]));
    mock.push_input_response(reply(idx, 1, &[0x00]));

    let mut dev = open(mock);
    let cfg = dev.as_keyboard_config_mut().unwrap();
    let current = cfg.read_keyboard_config().unwrap();
    assert!(current.fn_inversion);
    cfg.set_keyboard_config(&current.with_fn_inversion(false))
        .unwrap();
    assert_eq!(
        writes_to(&dev, idx).last().unwrap(),
        &frame(idx, 1, &[0x00])
    );
}

#[test]
fn legacy_fn_inversion_masks_low_nibble() {
    let idx = index_of(feature::FN_INVERSION);
    let mut mock = keyboard_mock(None, &[feature::FN_INVERSION]);
    mock.push_input_response(reply(idx, 0, &[0xF1]));
    mock.push_input_response(reply(idx, 0, &[0xF1]));
    mock.push_input_response(reply(idx, 1, &[0x00]));

    let mut dev = open(mock);
    let cfg = dev.as_keyboard_config_mut().unwrap();
    let current = cfg.read_keyboard_config().unwrap();
    assert!(current.fn_inversion);
    cfg.set_keyboard_config(&current.with_fn_inversion(false))
        .unwrap();
    assert_eq!(
        writes_to(&dev, idx).last().unwrap(),
        &frame(idx, 1, &[0x00])
    );
}

#[test]
fn disable_keys_by_usage_drives_game_mode_and_rejects_bad_usages() {
    let idx = index_of(feature::DISABLE_KEYS_BY_USAGE);
    let mut mock = keyboard_mock(Some(0x8B), &[feature::DISABLE_KEYS_BY_USAGE]);
    mock.push_input_response(reply(idx, 0, &[0x10]));
    // read, then enable game mode: rate read, fn 1
    mock.push_input_response(reply(IDX_REPORT_RATE, 1, &[0x01]));
    mock.push_input_response(reply(IDX_REPORT_RATE, 1, &[0x01]));
    mock.push_input_response(reply(idx, 1, &[]));
    // read, then disable game mode + change rate: rate read, fn 2, rate write
    mock.push_input_response(reply(IDX_REPORT_RATE, 1, &[0x01]));
    mock.push_input_response(reply(IDX_REPORT_RATE, 1, &[0x01]));
    mock.push_input_response(reply(idx, 2, &[]));
    mock.push_input_response(reply(IDX_REPORT_RATE, 2, &[0x02]));
    mock.push_input_response(reply(idx, 3, &[]));

    let mut dev = open(mock);
    assert!(dev.capabilities().has_keyboard_config());
    let cfg = dev.as_keyboard_config_mut().unwrap();
    let caps = cfg.keyboard_config_capabilities();
    assert!(caps.supports_game_mode);
    assert!(caps.supports_win_lock);
    assert!(!caps.supports_fn_inversion);
    assert_eq!(caps.supported_polling_rates.len(), 4);

    let current = cfg.read_keyboard_config().unwrap();
    assert!(!current.game_mode);
    cfg.set_keyboard_config(&current.with_game_mode(true))
        .unwrap();
    let current = cfg.read_keyboard_config().unwrap();
    assert!(current.game_mode && current.win_lock);
    let mut next = current.with_game_mode(false);
    next.polling_rate = PollingRateHz::Hz500;
    next.win_lock = false;
    cfg.set_keyboard_config(&next).unwrap();

    let writes = writes_to(&dev, idx);
    assert!(writes.contains(&frame(idx, 1, &[0xE3, 0xE7, 0x65])));
    assert!(writes.contains(&frame(idx, 2, &[0xE3, 0xE7, 0x65])));
    assert_eq!(
        writes_to(&dev, IDX_REPORT_RATE).last().unwrap(),
        &frame(IDX_REPORT_RATE, 2, &[0x02])
    );

    let before = write_count(&dev);
    assert!(dev.disable_keys_by_usage(&[0x04, 0x00]).is_err());
    assert!(dev.disable_keys_by_usage(&[]).is_err());
    assert!(dev.enable_keys_by_usage(&[0x00]).is_err());
    assert!(dev.disable_keys_by_usage(&[0x04; 17]).is_err());
    assert_eq!(write_count(&dev), before);

    dev.enable_all_keys().unwrap();
    assert_eq!(writes_to(&dev, idx).last().unwrap(), &frame(idx, 3, &[]));
}

fn push_control(mock: &mut MockHidTransport, info: [u8; 9]) {
    let idx = index_of(feature::REPROG_CONTROLS_V4);
    mock.push_input_response(reply(idx, 1, &info));
    mock.push_input_response(reply(idx, 2, &[info[0], info[1], 0x00, 0x00, 0x00, 0x00]));
}

#[test]
fn reprog_controls_v4_remaps_legal_targets_and_rejects_group_mask_violations() {
    let idx = index_of(feature::REPROG_CONTROLS_V4);
    let mut mock = keyboard_mock(None, &[feature::REPROG_CONTROLS_V4]);
    mock.push_input_response(reply(idx, 0, &[0x03]));
    // cid, tid, flags, pos, group, group_mask, flags_hi
    push_control(
        &mut mock,
        [0x00, 0x50, 0x00, 0x38, 0x00, 0x00, 0x01, 0x00, 0x00],
    );
    push_control(
        &mut mock,
        [0x00, 0xC3, 0x00, 0xA9, 0x30, 0x01, 0x03, 0x03, 0x00],
    );
    push_control(
        &mut mock,
        [0x00, 0xC4, 0x00, 0xAA, 0x10, 0x02, 0x02, 0x04, 0x00],
    );
    // read_keymap: C4 already reports a remap to C3
    mock.push_input_response(reply(idx, 2, &[0x00, 0x50, 0x00, 0x00, 0x00, 0x00]));
    mock.push_input_response(reply(idx, 2, &[0x00, 0xC3, 0x00, 0x00, 0x00, 0x00]));
    mock.push_input_response(reply(idx, 2, &[0x00, 0xC4, 0x00, 0x00, 0xC3, 0x00]));
    mock.push_input_response(reply(idx, 3, &[]));
    mock.push_input_response(reply(idx, 3, &[]));
    mock.push_input_response(reply(idx, 3, &[]));

    let mut dev = open(mock);
    assert!(dev.capabilities().has_keymap());
    let keymap = dev.as_keymap_mut().unwrap();
    let caps = keymap.keymap_capabilities();
    assert_eq!((caps.rows, caps.cols), (1, 3));
    assert_eq!(caps.layer_names, vec!["Controls".to_owned()]);

    let snap = keymap.read_keymap().unwrap();
    assert_eq!(
        snap.layers[0],
        vec![
            KeyCode::new(0x00, 0x50),
            KeyCode::new(0x00, 0xC3),
            KeyCode::new(0x00, 0xC3)
        ]
    );

    keymap
        .set_key_binding(0, KeyIndex::new(1), KeyCode::new(0x00, 0x50))
        .unwrap();

    let before = write_count(&dev);
    let keymap = dev.as_keymap_mut().unwrap();
    // C4's mask only allows group 3; 0x0050 is in group 1.
    assert!(
        keymap
            .set_key_binding(0, KeyIndex::new(2), KeyCode::new(0x00, 0x50))
            .is_err()
    );
    // 0x0050 is not reprogrammable.
    assert!(
        keymap
            .set_key_binding(0, KeyIndex::new(0), KeyCode::new(0x00, 0xC3))
            .is_err()
    );
    assert!(
        keymap
            .set_key_binding(1, KeyIndex::new(1), KeyCode::new(0x00, 0x50))
            .is_err()
    );
    assert!(
        keymap
            .set_key_binding(0, KeyIndex::new(3), KeyCode::new(0x00, 0x50))
            .is_err()
    );
    assert_eq!(write_count(&dev), before);

    dev.as_keymap_mut().unwrap().reset_keymap().unwrap();

    let writes: Vec<_> = writes_to(&dev, idx)
        .into_iter()
        .filter(|w| w[3] == 0x3C)
        .collect();
    assert_eq!(
        writes,
        vec![
            frame(idx, 3, &[0x00, 0xC3, 0x00, 0x00, 0x50]),
            frame(idx, 3, &[0x00, 0xC3, 0x00, 0x00, 0x00]),
            frame(idx, 3, &[0x00, 0xC4, 0x00, 0x00, 0x00]),
        ]
    );

    let controls = dev.controls.clone();
    assert!(controls.iter().all(|c| !c.is_remapped()));
    assert!(controls[1].is_legal_target(0x00C4, &controls));
    assert!(!controls[2].is_legal_target(0x0050, &controls));
}

#[test]
fn keyboard_probe_tolerates_error_frames_and_stops_on_timeout() {
    let desc = HidDeviceDescriptor::new(
        LOGITECH_VID,
        0xC33F,
        HIDPP_USAGE_PAGE_USB,
        0x0001,
        1,
        "mock://logitech/keyboard",
    );
    let mut mock = MockHidTransport::new(desc);
    mock.push_input_response(reply(0x00, 1, &[0x04, 0x02, 0x5A]));
    for _ in 0..8 {
        mock.push_input_response(reply(0x00, 0, &[0x00]));
    }
    // 0x8071 answered with a HID++ 2.0 error frame (invalid argument)
    let mut err = vec![0u8; 20];
    err[..6].copy_from_slice(&[0x11, DEV, 0xFF, 0x00, 0x0C, 0x02]);
    mock.push_input_response(err);
    // 0x8070 answers, then the queue runs dry (timeout) on 0x40A3

    mock.push_input_response(reply(0x00, 0, &[0x00]));

    let dev = open(mock);
    assert_eq!(dev.feature_indices.get(&feature::RGB_EFFECTS), Some(&0));
    assert_eq!(
        dev.feature_indices.get(&feature::COLOR_LED_EFFECTS),
        Some(&0)
    );
    assert!(
        !dev.feature_indices
            .contains_key(&feature::K375S_FN_INVERSION)
    );
    assert!(!dev.capabilities().has_rgb());
    assert!(!dev.capabilities().has_keyboard_config());
    assert!(!dev.capabilities().has_keymap());
}

#[test]
fn speed_period_mapping_round_trips() {
    for speed in [0u8, 1, 100, 200, 255] {
        let period = crate::protocol::speed_to_period_ms(speed);
        assert_eq!(crate::protocol::period_ms_to_speed(period), speed);
    }
}

#[test]
fn led_effect_encoders_match_documented_offsets_and_round_trip() {
    use crate::protocol::{decode_led_effect_params, encode_led_effect_params, led_effect};

    let cycle = RgbEffect::SpectrumCycle { speed: 100 };
    // speed 100 -> 7200 ms (0x1C20)
    assert_eq!(
        encode_led_effect_params(led_effect::CYCLE, &cycle, pct(70)).unwrap(),
        [0, 0, 0, 0, 0, 0x1C, 0x20, 70, 0, 0]
    );
    assert_eq!(
        encode_led_effect_params(led_effect::CYCLE_SATURATION, &cycle, pct(70)).unwrap(),
        [0, 0xFF, 0, 0, 0, 0, 0x1C, 0x20, 70, 0]
    );
    let wave = RgbEffect::Wave {
        speed: 100,
        direction: RgbDirection::BottomToTop,
    };
    assert_eq!(
        encode_led_effect_params(led_effect::WAVE, &wave, pct(100)).unwrap(),
        [0, 0, 0, 0, 0, 0, 0x1C, 0x20, 0, 7]
    );
    assert!(encode_led_effect_params(led_effect::WAVE, &cycle, pct(100)).is_err());

    let breathing = RgbEffect::Breathing {
        color: RgbColor::new(1, 2, 3),
        speed: 42,
    };
    for (id, effect, brightness) in [
        (led_effect::CYCLE, cycle, pct(70)),
        (led_effect::CYCLE_SATURATION, cycle, pct(70)),
        (led_effect::WAVE, wave, pct(100)),
        (led_effect::WAVE_SATURATION, wave, pct(35)),
        (led_effect::BREATHING, breathing, pct(55)),
        (led_effect::OFF, RgbEffect::Off, pct(100)),
    ] {
        let encoded = encode_led_effect_params(id, &effect, brightness).unwrap();
        assert_eq!(
            decode_led_effect_params(id, &encoded),
            Some((effect, brightness)),
            "effect 0x{id:02X}"
        );
    }
}

#[test]
fn dry_run_validates_but_suppresses_keyboard_writes() {
    let idx = index_of(feature::NEW_FN_INVERSION);
    let mut mock = keyboard_mock(None, &[feature::NEW_FN_INVERSION]);
    mock.push_input_response(reply(idx, 0, &[0x00, 0x00]));
    mock.push_input_response(reply(idx, 0, &[0x00, 0x00]));

    let mut dev = LogitechHidppDevice::open(mock, None, true, None).unwrap();
    let cfg = dev.as_keyboard_config_mut().unwrap();
    let current = cfg.read_keyboard_config().unwrap();
    cfg.set_keyboard_config(&current.with_fn_inversion(true))
        .unwrap();
    assert!(dev.disable_keys_by_usage(&[0x04]).is_err());

    let writes = writes_to(&dev, idx);
    assert_eq!(writes.len(), 2);
    assert!(writes.iter().all(|w| w[3] == 0x0C));
}

fn push_reprog_controls(mock: &mut MockHidTransport) {
    let idx = index_of(feature::REPROG_CONTROLS_V4);
    mock.push_input_response(reply(idx, 0, &[0x02]));
    push_control(mock, [0x00, 0xC3, 0x00, 0xA9, 0x10, 0x00, 0x01, 0x01, 0x00]);
    push_control(mock, [0x00, 0xC4, 0x00, 0xAA, 0x10, 0x01, 0x01, 0x01, 0x00]);
}

#[test]
fn typed_reprog_helpers_reread_controls_and_remap() {
    let idx = index_of(feature::REPROG_CONTROLS_V4);
    let mut mock = keyboard_mock(None, &[feature::REPROG_CONTROLS_V4]);
    push_reprog_controls(&mut mock);
    push_reprog_controls(&mut mock);
    mock.push_input_response(reply(idx, 3, &[]));

    let mut dev = open(mock);
    let controls = dev.read_reprog_controls().unwrap();
    assert_eq!(controls.len(), 2);
    assert_eq!(controls[1].cid, 0x00C4);
    assert_eq!(controls[1].task_id, 0x00AA);
    assert_eq!(controls[1].position, 0x01);
    assert!(controls[1].is_reprogrammable());

    let before = write_count(&dev);
    assert!(dev.remap_control(0x00C3, 0x1234).is_err());
    assert!(dev.remap_control(0x0099, 0x00C4).is_err());
    assert_eq!(write_count(&dev), before);

    dev.remap_control(0x00C3, 0x00C4).unwrap();
    assert_eq!(
        writes_to(&dev, idx).last().unwrap(),
        &frame(idx, 3, &[0x00, 0xC3, 0x00, 0x00, 0xC4])
    );
    assert!(dev.controls[0].is_remapped());
}

#[test]
fn failed_reprog_detail_read_disables_keymap_without_failing_open() {
    let mock = keyboard_mock(None, &[feature::REPROG_CONTROLS_V4]);
    let dev = open(mock);
    assert!(!dev.capabilities().has_keymap());
    assert!(dev.as_keymap().is_none());
}
