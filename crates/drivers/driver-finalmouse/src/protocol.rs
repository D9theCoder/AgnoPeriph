//! Finalmouse UltralightX & Starlight X 64-byte USB HID codec.
//!
//! Verified against `docs/protocols/finalmouse-hid.md` and
//! `OpenMouse-Project/mouse-protocol` (`src/finalmouse/index.ts`, `src/drivers/finalmouse/hid.ts`).

use agnoperiph_core::{
    BatteryStatus, ChargingState, ConnectionKind, CoreError, CoreResult, LiftOffDistance,
    PollingRateHz,
};
use agnoperiph_transport::{FixedReportValidator, ReportKind};

pub const FINALMOUSE_VID: u16 = 0x361D;
pub const FINALMOUSE_USAGE_PAGE: u16 = 0xFF00;
pub const FINALMOUSE_ULX_USAGE_ID: u16 = 0x0001;

/// Full `hidapi` packet length: 1-byte Report ID + 63-byte payload = 64 bytes.
pub const FINALMOUSE_PACKET_LEN: usize = 64;
pub const FINALMOUSE_MAX_PAYLOAD: usize = 60;

pub const DPI_MIN: u32 = 50;
pub const DPI_MAX: u32 = 26_000;
pub const DPI_STEP: u32 = 50;

pub mod report_id {
    pub const SLX_OUT: u8 = 0x01;
    pub const SLX_IN: u8 = 0x02;
    pub const ULX_DONGLE_OUT: u8 = 0x02;
    pub const ULX_DONGLE_IN: u8 = 0x03;
    pub const ULX_MAIN_OUT: u8 = 0x04;
    pub const ULX_MAIN_IN: u8 = 0x05;
}

pub mod cmd {
    pub const DONGLE_INFO: u8 = 10;
    pub const DPI: u8 = 16;
    pub const POLLING_RATE: u8 = 17;
    pub const MOTION_SYNC: u8 = 18;
    pub const LIFT_OFF_DISTANCE: u8 = 21;
    pub const PAW_LOD: u8 = 25;
    pub const PROFILE: u8 = 57;
    pub const WAKE_ALL: u8 = 96;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalmouseFamily {
    UltralightX,
    StarlightX,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalmouseProductProfile {
    pub pid: u16,
    pub model_name: &'static str,
    pub family: FinalmouseFamily,
    pub connection: ConnectionKind,
}

impl FinalmouseProductProfile {
    pub const fn tx_report_id(self) -> u8 {
        match self.family {
            FinalmouseFamily::UltralightX => report_id::ULX_MAIN_OUT,
            FinalmouseFamily::StarlightX => report_id::SLX_OUT,
        }
    }

    pub const fn rx_report_id(self) -> u8 {
        match self.family {
            FinalmouseFamily::UltralightX => report_id::ULX_MAIN_IN,
            FinalmouseFamily::StarlightX => report_id::SLX_IN,
        }
    }

    pub const fn dongle_tx_report_id(self) -> u8 {
        match self.family {
            FinalmouseFamily::UltralightX => report_id::ULX_DONGLE_OUT,
            FinalmouseFamily::StarlightX => report_id::SLX_OUT,
        }
    }
}

pub const FINALMOUSE_PRODUCTS: &[FinalmouseProductProfile] = &[
    FinalmouseProductProfile {
        pid: 0x0100,
        model_name: "Finalmouse UltralightX (Dongle)",
        family: FinalmouseFamily::UltralightX,
        connection: ConnectionKind::WirelessDongle24Ghz,
    },
    FinalmouseProductProfile {
        pid: 0x0102,
        model_name: "Finalmouse UltralightX (Wired)",
        family: FinalmouseFamily::UltralightX,
        connection: ConnectionKind::WiredUsb,
    },
    FinalmouseProductProfile {
        pid: 0x0300,
        model_name: "Finalmouse Starlight X (Dongle)",
        family: FinalmouseFamily::StarlightX,
        connection: ConnectionKind::WirelessDongle24Ghz,
    },
    FinalmouseProductProfile {
        pid: 0x0301,
        model_name: "Finalmouse Starlight X (Dongle)",
        family: FinalmouseFamily::StarlightX,
        connection: ConnectionKind::WirelessDongle24Ghz,
    },
    FinalmouseProductProfile {
        pid: 0x0310,
        model_name: "Finalmouse Starlight X (Wired)",
        family: FinalmouseFamily::StarlightX,
        connection: ConnectionKind::WiredUsb,
    },
    FinalmouseProductProfile {
        pid: 0x0311,
        model_name: "Finalmouse Starlight X (Wired)",
        family: FinalmouseFamily::StarlightX,
        connection: ConnectionKind::WiredUsb,
    },
];

pub fn lookup_product(pid: u16) -> Option<FinalmouseProductProfile> {
    FINALMOUSE_PRODUCTS.iter().copied().find(|p| p.pid == pid)
}

/// Encodes one 64-byte Finalmouse output report (`[report_id, 2 + len, 0x80 | cmd, len, payload...]`).
pub fn encode_report(
    report_id: u8,
    command: u8,
    payload: &[u8],
) -> CoreResult<[u8; FINALMOUSE_PACKET_LEN]> {
    if command > 0x7F {
        return Err(CoreError::Validation(format!(
            "Finalmouse command 0x{command:02X} exceeds 0x7F"
        )));
    }
    if payload.len() > FINALMOUSE_MAX_PAYLOAD {
        return Err(CoreError::OutOfRange {
            field: "finalmouse_payload_len",
            actual: payload.len().to_string(),
            expected: format!("0..={FINALMOUSE_MAX_PAYLOAD}"),
        });
    }
    let mut buf = [0u8; FINALMOUSE_PACKET_LEN];
    buf[0] = report_id;
    buf[1] = u8::try_from(2 + payload.len()).unwrap_or(2);
    buf[2] = 0x80 | command;
    buf[3] = u8::try_from(payload.len()).unwrap_or(0);
    buf[4..4 + payload.len()].copy_from_slice(payload);
    Ok(buf)
}

/// Strict pre-transmission validator for Finalmouse 64-byte output reports.
pub fn finalmouse_validator(family: FinalmouseFamily) -> FixedReportValidator {
    let allowed_ids = match family {
        FinalmouseFamily::UltralightX => vec![report_id::ULX_MAIN_OUT, report_id::ULX_DONGLE_OUT],
        FinalmouseFamily::StarlightX => vec![report_id::SLX_OUT],
    };
    FixedReportValidator::new(vec![FINALMOUSE_PACKET_LEN], allowed_ids).with_checksum(
        |kind, report| {
            if kind != ReportKind::Output {
                return Err(format!("Finalmouse requires Output reports, got {kind}"));
            }
            if (report[2] & 0x80) == 0 {
                return Err(format!(
                    "Finalmouse command byte 0x{:02X} missing 0x80 host bit",
                    report[2]
                ));
            }
            let payload_len = usize::from(report[3]);
            if usize::from(report[1]) != payload_len + 2 || payload_len > FINALMOUSE_MAX_PAYLOAD {
                return Err(format!(
                    "Finalmouse framing mismatch: inner_len={}, payload_len={payload_len}",
                    report[1]
                ));
            }
            Ok(())
        },
    )
}

/// Accumulated telemetry decoded from Finalmouse input report bursts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FinalmouseTelemetry {
    pub dpi: Option<u32>,
    pub polling_rate: Option<PollingRateHz>,
    pub battery_voltage_mv: Option<u16>,
    pub battery_percent: Option<u8>,
    pub battery_soc: Option<u8>,
    pub link_state: Option<u8>,
    pub charging_state: Option<u8>,
    pub lift_off_mm: Option<u8>,
    pub paw_lod_tenths_mm: Option<u8>,
    pub motion_sync: Option<bool>,
    pub mouse_firmware: Option<String>,
    pub dongle_usb_firmware: Option<String>,
    pub mouse_serial: Option<String>,
    pub profile_active: Option<u8>,
    pub profile_count: Option<u8>,
}

impl FinalmouseTelemetry {
    pub fn apply_report(&mut self, raw: &[u8]) -> bool {
        if raw.len() < 4 {
            return false;
        }
        let rid = raw[0];
        let is_mouse = rid == report_id::ULX_MAIN_IN || rid == report_id::SLX_IN;
        let is_dongle = rid == report_id::ULX_DONGLE_IN;
        if !is_mouse && !is_dongle {
            return false;
        }
        let inner_len = usize::from(raw[1]).min(raw.len() - 2);
        let body = &raw[2..2 + inner_len];
        if body.len() < 2 {
            return false;
        }
        let command = body[0];
        let payload_len = usize::from(body[1]).min(body.len() - 2);
        let payload = &body[2..2 + payload_len];

        if is_dongle {
            if command == cmd::DONGLE_INFO {
                self.dongle_usb_firmware = decode_ascii(payload);
                return true;
            }
            return false;
        }

        match command {
            3 | cmd::DPI if payload.len() >= 2 => {
                self.dpi = Some(u32::from(u16::from_le_bytes([payload[0], payload[1]])));
            }
            4 | cmd::POLLING_RATE if payload.len() >= 2 => {
                let hz = u16::from_le_bytes([payload[0], payload[1]]);
                if let Ok(rate) = PollingRateHz::from_hz(hz) {
                    self.polling_rate = Some(rate);
                }
            }
            5 if payload.len() >= 2 => {
                let mv = u16::from_le_bytes([payload[0], payload[1]]);
                self.battery_voltage_mv = Some(mv);
                self.battery_percent = Some(battery_percent_from_mv(mv));
            }
            12 => self.mouse_firmware = decode_ascii(payload),
            14 => self.mouse_serial = decode_ascii(payload),
            cmd::MOTION_SYNC if !payload.is_empty() => {
                self.motion_sync = Some(payload[0] == 1);
            }
            cmd::LIFT_OFF_DISTANCE if !payload.is_empty() => {
                self.lift_off_mm = Some(payload[0]);
            }
            cmd::PAW_LOD if !payload.is_empty() => {
                self.paw_lod_tenths_mm = decode_paw_lod_tenths(payload[0]);
            }
            36 if !payload.is_empty() => self.link_state = Some(payload[0]),
            37 if !payload.is_empty() => self.charging_state = Some(payload[0]),
            38 if payload.len() >= 3 => {
                self.battery_soc = Some(payload[0].min(100));
                self.battery_voltage_mv = Some(u16::from_le_bytes([payload[1], payload[2]]));
            }
            cmd::PROFILE if payload.len() >= 2 => {
                self.profile_active = Some(payload[0]);
                self.profile_count = Some(payload[1]);
            }
            _ => return false,
        }
        true
    }

    pub fn battery_status(&self) -> BatteryStatus {
        let pct = self.battery_soc.or(self.battery_percent);
        let state = match self.charging_state {
            Some(1) => ChargingState::Charging,
            Some(_) => ChargingState::Discharging,
            None => ChargingState::Unknown,
        };
        let connected = self.link_state != Some(0);
        BatteryStatus::new(pct, self.battery_voltage_mv, state, connected)
    }

    pub fn effective_lod(&self) -> Option<LiftOffDistance> {
        if let Some(tenths) = self.paw_lod_tenths_mm {
            return Some(LiftOffDistance::TenthsMm(tenths));
        }
        match self.lift_off_mm {
            Some(1) => Some(LiftOffDistance::TenthsMm(10)),
            Some(2) => Some(LiftOffDistance::TenthsMm(20)),
            _ => None,
        }
    }
}

/// Finalmouse ULX battery-voltage curve from `xpanel` (integer millivolt interpolation).
pub fn battery_percent_from_mv(mv: u16) -> u8 {
    const POINTS: &[(u32, u32)] = &[
        (3000, 0),
        (3620, 5),
        (3660, 10),
        (3740, 25),
        (3880, 50),
        (4170, 75),
        (4380, 100),
    ];
    let mv = u32::from(mv);
    if mv <= POINTS[0].0 {
        return 0;
    }
    if mv >= POINTS[POINTS.len() - 1].0 {
        return 100;
    }
    for win in POINTS.windows(2) {
        let (v_lo, p_lo) = win[0];
        let (v_hi, p_hi) = win[1];
        if mv >= v_lo && mv <= v_hi {
            let span = v_hi - v_lo;
            let pct = p_lo + ((mv - v_lo) * (p_hi - p_lo) + span / 2) / span;
            return u8::try_from(pct.min(100)).unwrap_or(100);
        }
    }
    0
}

pub fn decode_paw_lod_tenths(raw: u8) -> Option<u8> {
    match raw {
        1 => Some(7),
        2 => Some(10),
        3 => Some(20),
        7..=17 => Some(raw),
        _ => None,
    }
}

pub fn encode_paw_lod(lod: LiftOffDistance) -> CoreResult<u8> {
    match lod {
        LiftOffDistance::TenthsMm(10) | LiftOffDistance::Medium => Ok(2),
        LiftOffDistance::TenthsMm(20) | LiftOffDistance::High => Ok(3),
        LiftOffDistance::Low => Ok(7),
        LiftOffDistance::TenthsMm(t @ 7..=17) => Ok(t),
        other @ LiftOffDistance::TenthsMm(_) => Err(CoreError::OutOfRange {
            field: "lod",
            actual: other.to_string(),
            expected: "0.7..=1.7 mm or 2.0 mm".to_owned(),
        }),
    }
}

fn decode_ascii(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]).trim().to_owned();
    (!text.is_empty()).then_some(text)
}
