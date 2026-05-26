// crates/interoception/src/power.rs
//! Power and attention sensors — E5.7 Story S5.7.3.
//!
//! Both sensors are gated by an explicit opt-in flag. When disabled they
//! return conservative sentinels (AC power, user present) so the rest of the
//! system degrades gracefully on hosts where sysfs or windowing-system APIs
//! are unavailable.
//!
//! ## Platform support
//!
//! **Power:** Linux hosts expose battery state via
//! `/sys/class/power_supply/<name>/{type,status,capacity}`. On any other
//! platform — or when the directory is missing or permission-restricted — the
//! sensor falls back to `PowerReading::ac_power()` (`power_budget_scalar = 1.0`).
//!
//! **Attention:** Platform-specific idle detection (X11 MIT-SHM idle timer,
//! D-Bus `org.freedesktop.ScreenSaver`) is deferred until E5.7 integrates on
//! a desktop host. The current implementation returns `user_present()` when
//! enabled and data is unavailable (conservative — assumes user is active).

#![forbid(unsafe_code)]

// ── Power reading ─────────────────────────────────────────────────────────────

/// Battery / AC power state snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerReading {
    /// `true` when running on battery; `false` when on AC power.
    pub on_battery: bool,
    /// Battery charge fraction in `[0, 1]`.
    ///
    /// Meaningful only when `on_battery = true`. Set to `1.0` on AC power.
    pub charge_fraction: f32,
    /// `true` when the battery is actively being charged.
    pub is_charging: bool,
}

impl PowerReading {
    /// Sentinel reading for hosts without a battery (AC-only).
    pub fn ac_power() -> Self {
        Self {
            on_battery: false,
            charge_fraction: 1.0,
            is_charging: false,
        }
    }

    /// Construct an on-battery reading with the given charge fraction.
    ///
    /// `charge_fraction` is clamped to `[0, 1]`.
    pub fn battery(charge_fraction: f32, is_charging: bool) -> Self {
        Self {
            on_battery: true,
            charge_fraction: charge_fraction.clamp(0.0, 1.0),
            is_charging,
        }
    }

    /// Normalised power budget scalar.
    ///
    /// - Returns `1.0` when on AC power (unlimited budget).
    /// - Returns `charge_fraction` when on battery.
    pub fn power_budget_scalar(&self) -> f32 {
        if self.on_battery {
            self.charge_fraction
        } else {
            1.0
        }
    }
}

// ── Power sensor ──────────────────────────────────────────────────────────────

/// Configuration for the power sensor.
#[derive(Debug, Clone, Default)]
pub struct PowerConfig {
    /// Whether to attempt live battery readings via sysfs.
    ///
    /// When `false`, [`PowerSensor::read`] always returns
    /// [`PowerReading::ac_power()`].
    pub enabled: bool,
}

/// Power state sensor (S5.7.3).
///
/// Reads battery / AC state from the host. All platform-specific I/O is
/// isolated to `try_read_sysfs`; callers interact only via `read()` and
/// `power_budget_scalar()`.
#[derive(Debug, Clone)]
pub struct PowerSensor {
    config: PowerConfig,
}

impl PowerSensor {
    /// Creates a sensor with the given configuration.
    pub fn new(config: PowerConfig) -> Self {
        Self { config }
    }

    /// Creates a sensor with opt-in disabled (always returns AC power).
    pub fn disabled() -> Self {
        Self::new(PowerConfig { enabled: false })
    }

    /// Returns the current power reading.
    ///
    /// When disabled, always returns [`PowerReading::ac_power()`].
    /// When enabled, attempts sysfs; falls back to AC on any error.
    pub fn read(&self) -> PowerReading {
        if !self.config.enabled {
            return PowerReading::ac_power();
        }
        Self::try_read_sysfs().unwrap_or_else(PowerReading::ac_power)
    }

    /// Returns the normalised power budget scalar.
    pub fn power_budget_scalar(&self) -> f32 {
        self.read().power_budget_scalar()
    }

    /// Reads battery state from the Linux sysfs power supply interface.
    ///
    /// Returns `None` if sysfs is absent, the directory is empty, any read
    /// fails, or no `Battery`-type supply is found.
    fn try_read_sysfs() -> Option<PowerReading> {
        use std::fs;
        let dir = fs::read_dir("/sys/class/power_supply").ok()?;
        for entry in dir.flatten() {
            let path = entry.path();
            let type_str = fs::read_to_string(path.join("type")).ok()?;
            if type_str.trim() != "Battery" {
                continue;
            }
            let status = fs::read_to_string(path.join("status")).unwrap_or_default();
            let status = status.trim();
            let cap_str = fs::read_to_string(path.join("capacity")).ok()?;
            let capacity: f32 = cap_str.trim().parse().ok()?;
            let charge_fraction = (capacity / 100.0).clamp(0.0, 1.0);
            let is_charging = status == "Charging";
            let on_battery = status != "Full" && !is_charging;
            return Some(PowerReading {
                on_battery,
                charge_fraction,
                is_charging,
            });
        }
        None
    }
}

// ── Attention reading ─────────────────────────────────────────────────────────

/// User presence / attention snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct AttentionReading {
    /// `true` when the agent's process is in the foreground.
    pub is_foreground: bool,
    /// Seconds since the last detectable user input event.
    ///
    /// When data is unavailable, defaults to `0.0` (user is immediately active).
    pub idle_secs: f64,
}

impl AttentionReading {
    /// Sentinel: user is actively present (conservative — assumes engagement).
    pub fn user_present() -> Self {
        Self {
            is_foreground: true,
            idle_secs: 0.0,
        }
    }

    /// Sentinel: fully idle / backgrounded agent.
    pub fn idle() -> Self {
        Self {
            is_foreground: false,
            idle_secs: 300.0,
        }
    }

    /// Normalised attention demand scalar in `[0, 1]`.
    ///
    /// - `1.0` → user is actively engaged, zero idle time.
    /// - `0.0` → agent is in the background or user has been idle for
    ///   `≥ idle_ceiling_secs`.
    ///
    /// The scalar decays linearly from 1.0 at 0 idle seconds down to 0.0
    /// at `idle_ceiling_secs` (default: 300 s / 5 minutes).
    pub fn attention_demand_scalar(&self, idle_ceiling_secs: f64) -> f32 {
        if !self.is_foreground {
            return 0.0;
        }
        let ceiling = idle_ceiling_secs.max(1.0);
        (1.0 - (self.idle_secs / ceiling).min(1.0)) as f32
    }
}

// ── Attention sensor ──────────────────────────────────────────────────────────

/// Configuration for the attention sensor.
#[derive(Debug, Clone)]
pub struct AttentionConfig {
    /// Whether to attempt reading from platform idle APIs.
    ///
    /// When `false`, [`AttentionSensor::read`] always returns
    /// [`AttentionReading::user_present()`].
    pub enabled: bool,
    /// Seconds of inactivity at which attention demand reaches zero.
    pub idle_ceiling_secs: f64,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            idle_ceiling_secs: 300.0,
        }
    }
}

/// Attention / user-presence sensor (S5.7.3).
#[derive(Debug, Clone)]
pub struct AttentionSensor {
    config: AttentionConfig,
}

impl AttentionSensor {
    /// Creates a sensor with the given configuration.
    pub fn new(config: AttentionConfig) -> Self {
        Self { config }
    }

    /// Creates a sensor with opt-in disabled (always returns user-present).
    pub fn disabled() -> Self {
        Self::new(AttentionConfig::default())
    }

    /// Returns the current attention reading.
    ///
    /// When disabled, always returns [`AttentionReading::user_present()`].
    /// When enabled, attempts a platform query; falls back to `user_present`
    /// on any error.
    pub fn read(&self) -> AttentionReading {
        if !self.config.enabled {
            return AttentionReading::user_present();
        }
        // Platform idle detection (X11 / D-Bus / macOS / Windows) deferred
        // until the hosted target is running on a full desktop environment.
        // The conservative fallback (user is present) is correct here: it
        // causes the gate to behave more responsively, not less.
        AttentionReading::user_present()
    }

    /// Returns the normalised attention demand scalar in `[0, 1]`.
    pub fn attention_demand_scalar(&self) -> f32 {
        self.read()
            .attention_demand_scalar(self.config.idle_ceiling_secs)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── PowerReading ─────────────────────────────────────────────────────────

    #[test]
    fn ac_power_reading_gives_full_budget_scalar() {
        assert_eq!(PowerReading::ac_power().power_budget_scalar(), 1.0);
    }

    #[test]
    fn battery_reading_budget_equals_charge_fraction() {
        let r = PowerReading::battery(0.4, false);
        assert!((r.power_budget_scalar() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn battery_reading_clamps_charge_above_one() {
        assert_eq!(PowerReading::battery(1.5, false).charge_fraction, 1.0);
    }

    #[test]
    fn battery_reading_clamps_charge_below_zero() {
        assert_eq!(PowerReading::battery(-0.1, false).charge_fraction, 0.0);
    }

    #[test]
    fn charging_battery_has_on_battery_true() {
        let r = PowerReading::battery(0.8, true);
        assert!(r.on_battery);
        assert!(r.is_charging);
    }

    // ── PowerSensor ──────────────────────────────────────────────────────────

    #[test]
    fn disabled_power_sensor_always_returns_ac_power() {
        let sensor = PowerSensor::disabled();
        assert_eq!(sensor.read(), PowerReading::ac_power());
        assert_eq!(sensor.power_budget_scalar(), 1.0);
    }

    #[test]
    fn enabled_power_sensor_returns_a_valid_reading() {
        // In CI there is no battery; sysfs fallback gives AC power.
        let sensor = PowerSensor::new(PowerConfig { enabled: true });
        let r = sensor.read();
        assert!(
            (0.0..=1.0).contains(&r.charge_fraction),
            "charge_fraction must be in [0,1]: {}",
            r.charge_fraction
        );
        assert!((0.0..=1.0).contains(&sensor.power_budget_scalar()));
    }

    // ── AttentionReading ─────────────────────────────────────────────────────

    #[test]
    fn user_present_reading_has_maximum_attention_demand() {
        assert_eq!(
            AttentionReading::user_present().attention_demand_scalar(300.0),
            1.0
        );
    }

    #[test]
    fn idle_reading_has_zero_attention_demand() {
        assert_eq!(
            AttentionReading::idle().attention_demand_scalar(300.0),
            0.0,
            "background agent has no attention demand regardless of idle_secs"
        );
    }

    #[test]
    fn attention_decays_linearly_with_idle_time() {
        let r = AttentionReading {
            is_foreground: true,
            idle_secs: 150.0,
        };
        let demand = r.attention_demand_scalar(300.0);
        assert!(
            (demand - 0.5).abs() < 1e-6,
            "halfway idle → 0.5, got {demand}"
        );
    }

    #[test]
    fn attention_clamps_to_zero_when_idle_exceeds_ceiling() {
        let r = AttentionReading {
            is_foreground: true,
            idle_secs: 600.0,
        };
        assert_eq!(r.attention_demand_scalar(300.0), 0.0);
    }

    #[test]
    fn attention_demand_is_zero_when_not_foreground_regardless_of_idle() {
        let r = AttentionReading {
            is_foreground: false,
            idle_secs: 0.0,
        };
        assert_eq!(r.attention_demand_scalar(300.0), 0.0);
    }

    // ── AttentionSensor ──────────────────────────────────────────────────────

    #[test]
    fn disabled_attention_sensor_always_returns_user_present() {
        let sensor = AttentionSensor::disabled();
        assert_eq!(sensor.read(), AttentionReading::user_present());
        assert_eq!(sensor.attention_demand_scalar(), 1.0);
    }
}
