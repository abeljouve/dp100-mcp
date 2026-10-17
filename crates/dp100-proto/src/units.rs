//! Strongly typed physical quantities as encoded on the wire.

use core::fmt;

/// Error converting a floating-point engineering value to a wire unit.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum UnitError {
    #[error("{0} is not a finite number")]
    NotFinite(f64),
    #[error("{value} {unit} is negative")]
    Negative { value: f64, unit: &'static str },
    #[error("{value} {unit} does not fit the 16-bit wire field")]
    Overflow { value: f64, unit: &'static str },
}

fn scale(value: f64, factor: f64, unit: &'static str) -> Result<u16, UnitError> {
    if !value.is_finite() {
        return Err(UnitError::NotFinite(value));
    }
    if value < 0.0 {
        return Err(UnitError::Negative { value, unit });
    }
    let raw = (value * factor).round();
    if raw > f64::from(u16::MAX) {
        return Err(UnitError::Overflow { value, unit });
    }
    // In range [0, 65535] and integral after the checks above.
    Ok(raw as u16)
}

macro_rules! unit {
    ($(#[$m:meta])* $name:ident($inner:ty), $suffix:literal, $factor:expr, $to:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        pub struct $name(pub $inner);

        impl $name {
            /// Value in base SI-ish units (V, A, °C, W).
            pub fn $to(self) -> f64 {
                f64::from(self.0) / $factor
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{} {}", self.0, $suffix)
            }
        }

        impl From<$inner> for $name {
            fn from(v: $inner) -> Self {
                Self(v)
            }
        }
    };
}

unit!(
    /// Voltage in millivolts.
    Millivolts(u16), "mV", 1000.0, volts
);
unit!(
    /// Current in milliamps.
    Milliamps(u16), "mA", 1000.0, amps
);
unit!(
    /// Temperature in tenths of a degree Celsius.
    DeciCelsius(i16), "d°C", 10.0, celsius
);
unit!(
    /// Power in tenths of a watt (OPP setting).
    DeciWatts(u16), "dW", 10.0, watts
);

impl Millivolts {
    /// Convert volts to millivolts (rounded to the nearest mV).
    pub fn from_volts(v: f64) -> Result<Self, UnitError> {
        scale(v, 1000.0, "V").map(Self)
    }
}

impl Milliamps {
    /// Convert amps to milliamps (rounded to the nearest mA).
    pub fn from_amps(a: f64) -> Result<Self, UnitError> {
        scale(a, 1000.0, "A").map(Self)
    }
}

impl DeciWatts {
    /// Convert watts to 0.1 W steps (rounded).
    pub fn from_watts(w: f64) -> Result<Self, UnitError> {
        scale(w, 10.0, "W").map(Self)
    }
}

/// Power in milliwatts computed from a voltage and a current.
pub fn milliwatts(v: Millivolts, i: Milliamps) -> f64 {
    f64::from(v.0) * f64::from(i.0) / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(Millivolts::from_volts(5.0).unwrap(), Millivolts(5000));
        assert_eq!(Millivolts::from_volts(3.3004).unwrap(), Millivolts(3300));
        assert_eq!(Milliamps::from_amps(0.5).unwrap(), Milliamps(500));
        assert_eq!(DeciWatts::from_watts(105.0).unwrap(), DeciWatts(1050));
        assert!((Millivolts(4996).volts() - 4.996).abs() < 1e-9);
        assert!((DeciCelsius(284).celsius() - 28.4).abs() < 1e-9);
        assert!(Millivolts::from_volts(-0.1).is_err());
        assert!(Millivolts::from_volts(f64::NAN).is_err());
        assert!(Millivolts::from_volts(70.0).is_err());
        assert_eq!(milliwatts(Millivolts(5005), Milliamps(23)), 115.115);
    }
}
