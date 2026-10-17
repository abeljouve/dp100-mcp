//! Safety limits enforced on the host before anything is sent.
//! Requests outside the limits are refused, never clamped.

use dp100_proto::{BasicInfo, Milliamps, Millivolts, device_limits};
use serde::{Deserialize, Serialize};

/// Why a request was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LimitError {
    #[error("voltage {} V exceeds the configured maximum {} V", .requested.volts(), .max.volts())]
    VoltageAboveMax { requested: Millivolts, max: Millivolts },
    #[error("current {} A exceeds the configured maximum {} A", .requested.amps(), .max.amps())]
    CurrentAboveMax { requested: Milliamps, max: Milliamps },
    #[error(
        "voltage {} V exceeds the regulator ceiling {} V = min(vo_max {} V, Vin {} V − dropout {} V)",
        .requested.volts(), .ceiling.volts(), .vo_max.volts(), .vin.volts(), .dropout.volts()
    )]
    VoltageAboveInput {
        requested: Millivolts,
        ceiling: Millivolts,
        vin: Millivolts,
        vo_max: Millivolts,
        dropout: Millivolts,
    },
    #[error("voltage {} V is above the OVP threshold {} V of the active set (the output would trip)", .requested.volts(), .ovp.volts())]
    VoltageAboveOvp { requested: Millivolts, ovp: Millivolts },
    #[error("current limit {} A is above the OCP threshold {} A of the active set", .requested.amps(), .ocp.amps())]
    CurrentAboveOcp { requested: Milliamps, ocp: Milliamps },
    #[error("OVP {} V is outside the device range (max {} V)", .0.volts(), device_limits::MAX_OVP.volts())]
    OvpOutOfRange(Millivolts),
    #[error("OCP {} A is outside the device range (max {} A)", .0.amps(), device_limits::MAX_OCP.amps())]
    OcpOutOfRange(Milliamps),
}

/// Effective limits, in wire units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_voltage: Millivolts,
    pub max_current: Milliamps,
    /// Margin kept between Vin and Vout on top of the firmware's `vo_max`.
    pub vin_dropout: Millivolts,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_voltage: device_limits::MAX_VOUT,
            max_current: device_limits::MAX_IOUT,
            vin_dropout: Millivolts(DEFAULT_DROPOUT_MV),
        }
    }
}

/// Default buck-regulator margin (docs/PROTOCOL.md §7).
pub const DEFAULT_DROPOUT_MV: u16 = 500;

/// Limits as written in a TOML file or on the command line (engineering units).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    /// Maximum output voltage in volts (≤ 30).
    pub max_voltage_v: Option<f64>,
    /// Maximum current limit in amps (≤ 5).
    pub max_current_a: Option<f64>,
    /// Vin − Vout margin in millivolts (default 500).
    pub vin_dropout_mv: Option<u16>,
}

impl LimitsConfig {
    /// Overlay `other` (e.g. CLI flags) on top of `self` (e.g. a file).
    pub fn merged(self, other: LimitsConfig) -> LimitsConfig {
        LimitsConfig {
            max_voltage_v: other.max_voltage_v.or(self.max_voltage_v),
            max_current_a: other.max_current_a.or(self.max_current_a),
            vin_dropout_mv: other.vin_dropout_mv.or(self.vin_dropout_mv),
        }
    }

    /// Validate and convert. Values beyond the device rating are an error.
    pub fn resolve(&self) -> Result<Limits, String> {
        let d = Limits::default();
        let max_voltage = match self.max_voltage_v {
            None => d.max_voltage,
            Some(v) => Millivolts::from_volts(v).map_err(|e| format!("max voltage: {e}"))?,
        };
        let max_current = match self.max_current_a {
            None => d.max_current,
            Some(a) => Milliamps::from_amps(a).map_err(|e| format!("max current: {e}"))?,
        };
        if max_voltage > device_limits::MAX_VOUT {
            return Err(format!("max voltage {} V exceeds the DP100 rating of 30 V", max_voltage.volts()));
        }
        if max_current > device_limits::MAX_IOUT {
            return Err(format!("max current {} A exceeds the DP100 rating of 5 A", max_current.amps()));
        }
        Ok(Limits {
            max_voltage,
            max_current,
            vin_dropout: Millivolts(self.vin_dropout_mv.unwrap_or(DEFAULT_DROPOUT_MV)),
        })
    }
}

impl Limits {
    /// Highest Vout the regulator can deliver now: `min(vo_max, Vin − dropout)`.
    pub fn vout_ceiling(&self, t: &BasicInfo) -> Millivolts {
        Millivolts(t.vo_max.0.min(t.vin.0.saturating_sub(self.vin_dropout.0)))
    }

    /// Configured maxima only (no live telemetry needed).
    pub fn check_static(&self, v: Millivolts, i: Milliamps) -> Result<(), LimitError> {
        if v > self.max_voltage {
            return Err(LimitError::VoltageAboveMax { requested: v, max: self.max_voltage });
        }
        if i > self.max_current {
            return Err(LimitError::CurrentAboveMax { requested: i, max: self.max_current });
        }
        Ok(())
    }

    /// `v ≤ min(vo_max, Vin − dropout)` using fresh telemetry.
    pub fn check_input(&self, v: Millivolts, t: &BasicInfo) -> Result<(), LimitError> {
        let ceiling = self.vout_ceiling(t);
        if v > ceiling {
            return Err(LimitError::VoltageAboveInput {
                requested: v,
                ceiling,
                vin: t.vin,
                vo_max: t.vo_max,
                dropout: self.vin_dropout,
            });
        }
        Ok(())
    }

    /// Setpoints must sit below the set's own protections.
    pub fn check_protections(v: Millivolts, i: Milliamps, ovp: Millivolts, ocp: Milliamps) -> Result<(), LimitError> {
        if ovp > device_limits::MAX_OVP {
            return Err(LimitError::OvpOutOfRange(ovp));
        }
        if ocp > device_limits::MAX_OCP {
            return Err(LimitError::OcpOutOfRange(ocp));
        }
        if v >= ovp {
            return Err(LimitError::VoltageAboveOvp { requested: v, ovp });
        }
        if i > ocp {
            return Err(LimitError::CurrentAboveOcp { requested: i, ocp });
        }
        Ok(())
    }

    /// Every check that applies to a live output setting.
    pub fn check_output(
        &self,
        v: Millivolts,
        i: Milliamps,
        ovp: Millivolts,
        ocp: Milliamps,
        t: &BasicInfo,
    ) -> Result<(), LimitError> {
        self.check_static(v, i)?;
        Self::check_protections(v, i, ovp, ocp)?;
        self.check_input(v, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dp100_proto::DeciCelsius;

    fn telemetry(vin: u16, vo_max: u16) -> BasicInfo {
        BasicInfo {
            vin: Millivolts(vin),
            vout: Millivolts(0),
            iout: Milliamps(0),
            vo_max: Millivolts(vo_max),
            temp1: DeciCelsius(250),
            temp2: DeciCelsius(250),
            dc_5v: Millivolts(5000),
            out_mode_raw: 2,
            work_state_raw: 0,
        }
    }

    #[test]
    fn ceiling_uses_min_of_vo_max_and_dropout() {
        let l = Limits::default();
        // HW values from 2026-10-17: Vin 15.223 V, vo_max 14.790 V → 14.723 V.
        assert_eq!(l.vout_ceiling(&telemetry(15223, 14790)), Millivolts(14723));
        // weigu: Vin 20.168 V, vo_max 19.550 V → vo_max wins.
        assert_eq!(l.vout_ceiling(&telemetry(20168, 19550)), Millivolts(19550));
        assert_eq!(l.vout_ceiling(&telemetry(300, 0)), Millivolts(0));
    }

    #[test]
    fn refuses_instead_of_clamping() {
        let l = LimitsConfig { max_voltage_v: Some(12.0), max_current_a: Some(1.0), vin_dropout_mv: None }
            .resolve()
            .unwrap();
        let t = telemetry(15223, 14790);
        assert!(l.check_output(Millivolts(5000), Milliamps(500), Millivolts(30500), Milliamps(5050), &t).is_ok());
        assert!(matches!(l.check_static(Millivolts(12001), Milliamps(0)), Err(LimitError::VoltageAboveMax { .. })));
        assert!(matches!(l.check_static(Millivolts(0), Milliamps(1001)), Err(LimitError::CurrentAboveMax { .. })));
        let l = Limits::default();
        assert!(matches!(
            l.check_input(Millivolts(14724), &t),
            Err(LimitError::VoltageAboveInput { ceiling: Millivolts(14723), .. })
        ));
        assert!(matches!(
            Limits::check_protections(Millivolts(5000), Milliamps(100), Millivolts(5000), Milliamps(5050)),
            Err(LimitError::VoltageAboveOvp { .. })
        ));
        assert!(matches!(
            Limits::check_protections(Millivolts(5000), Milliamps(600), Millivolts(6000), Milliamps(500)),
            Err(LimitError::CurrentAboveOcp { .. })
        ));
    }

    #[test]
    fn config_resolution() {
        assert_eq!(LimitsConfig::default().resolve().unwrap(), Limits::default());
        assert!(LimitsConfig { max_voltage_v: Some(31.0), ..Default::default() }.resolve().is_err());
        assert!(LimitsConfig { max_current_a: Some(-1.0), ..Default::default() }.resolve().is_err());
        let file = LimitsConfig { max_voltage_v: Some(12.0), max_current_a: Some(2.0), vin_dropout_mv: Some(800) };
        let cli = LimitsConfig { max_voltage_v: Some(5.0), ..Default::default() };
        let m = file.merged(cli).resolve().unwrap();
        assert_eq!((m.max_voltage, m.max_current, m.vin_dropout), (Millivolts(5000), Milliamps(2000), Millivolts(800)));
    }
}
