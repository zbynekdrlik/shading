//! The relay's camera transport: shell out to the `gphoto2` CLI over USB-PTP.
//!
//! Design decision (issue 808 design comment, Prístup 1): the relay drives the camera
//! by spawning the system `gphoto2` binary — NOT a build-time `libgphoto2` FFI binding.
//! This keeps the crate free of any C build-time dependency, so it cross-compiles cleanly
//! for ARM (Pi Zero 2 W, the handheld SBC relay), and it reuses the exact gphoto2
//! semantics the dev2 MVP verified. The `Gphoto2Runner` trait is the seam: `Gphoto2Cli`
//! is the real impl, and tests inject a fake so every path is exercised without a camera.

use anyhow::{bail, Context, Result};
use bkshading_proto::mapping::{parse_choices, DEFAULT_FPS100};
use bkshading_proto::read::{fps_supported, params_and_caps, plan_writes, RawConfigs};
use bkshading_proto::wire::{RelayState, SetRequest};

/// The seam over the `gphoto2` CLI. Blocking (std process); handlers call it via
/// `spawn_blocking`. `Send + Sync` so it can live behind an `Arc` shared across tasks.
pub trait Gphoto2Runner: Send + Sync {
    /// `gphoto2 --auto-detect` stdout.
    fn auto_detect(&self) -> Result<String>;
    /// `gphoto2 --get-config <key>` stdout.
    fn get_config(&self, key: &str) -> Result<String>;
    /// `gphoto2 --set-config <key>=<value>`.
    fn set_config(&self, key: &str, value: &str) -> Result<()>;
}

/// Real transport: spawns the `gphoto2` binary.
pub struct Gphoto2Cli {
    pub binary: String,
}

impl Default for Gphoto2Cli {
    fn default() -> Self {
        Gphoto2Cli {
            binary: "gphoto2".to_string(),
        }
    }
}

impl Gphoto2Runner for Gphoto2Cli {
    fn auto_detect(&self) -> Result<String> {
        let out = std::process::Command::new(&self.binary)
            .arg("--auto-detect")
            .output()
            .with_context(|| format!("spawn {} --auto-detect", self.binary))?;
        if !out.status.success() {
            bail!(
                "gphoto2 --auto-detect failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn get_config(&self, key: &str) -> Result<String> {
        let out = std::process::Command::new(&self.binary)
            .arg("--get-config")
            .arg(key)
            .output()
            .with_context(|| format!("spawn {} --get-config {}", self.binary, key))?;
        if !out.status.success() {
            bail!(
                "gphoto2 --get-config {} failed: {}",
                key,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn set_config(&self, key: &str, value: &str) -> Result<()> {
        let out = std::process::Command::new(&self.binary)
            .arg("--set-config")
            .arg(format!("{key}={value}"))
            .output()
            .with_context(|| format!("spawn {} --set-config {}={}", self.binary, key, value))?;
        if !out.status.success() {
            bail!(
                "gphoto2 --set-config {}={} failed: {}",
                key,
                value,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

/// Parses the first camera model out of `gphoto2 --auto-detect` table output. The table is
/// `Model<ws>Port` with a `---` separator line; each data row ends in a `usb:BBB,DDD` port
/// token, so the model is the row with that token stripped. Returns `None` if no camera row
/// is present (camera absent / unplugged). Pure — unit-tested without a camera.
pub fn parse_first_model(output: &str) -> Option<String> {
    for raw in output.lines() {
        let line = raw.trim_end();
        // A data row ends in a port token; the header/separator rows do not.
        if let Some(idx) = line.rfind("usb:") {
            let model = line[..idx].trim();
            if !model.is_empty() {
                return Some(model.to_string());
            }
        }
    }
    None
}

/// One camera the relay owns, driven through a [`Gphoto2Runner`].
pub struct CameraSession {
    runner: Box<dyn Gphoto2Runner>,
    version: String,
}

impl CameraSession {
    pub fn new(runner: Box<dyn Gphoto2Runner>, version: impl Into<String>) -> Self {
        CameraSession {
            runner,
            version: version.into(),
        }
    }

    pub fn version(&self) -> String {
        self.version.clone()
    }

    /// Model string of the attached camera, or `None` if not detected.
    pub fn detect(&self) -> Option<String> {
        self.runner
            .auto_detect()
            .ok()
            .and_then(|o| parse_first_model(&o))
    }

    fn read_raw(&self) -> Result<RawConfigs> {
        Ok(RawConfigs {
            iso: self.runner.get_config("iso")?,
            fnumber: self.runner.get_config("f-number")?,
            shutter_angle: self.runner.get_config("d002")?,
            kelvin: self.runner.get_config("d004")?,
            tint: self.runner.get_config("d005")?,
            sensor_fps: self.runner.get_config("d006")?,
            project_fps: self.runner.get_config("d007")?,
        })
    }

    /// Reads the camera's live shading state. Never panics: a detect miss or a gphoto2
    /// read error degrades to an offline [`RelayState`], the server-is-truth model.
    pub fn read_state(&self) -> RelayState {
        let camera = self.detect();
        if camera.is_none() {
            return RelayState::offline(self.version.clone());
        }
        match self.read_raw() {
            Ok(raw) => {
                let (params, caps) = params_and_caps(&raw);
                RelayState {
                    online: true,
                    camera,
                    params,
                    caps: Some(caps),
                    fps_supported: fps_supported(&raw),
                    version: self.version.clone(),
                }
            }
            Err(_) => RelayState {
                camera,
                ..RelayState::offline(self.version.clone())
            },
        }
    }

    /// Applies a shading write request. Returns the number of gphoto2 `set-config` writes
    /// performed. Aperture is planned against the camera's live f-number choices and the
    /// live fps (for the shutter angle), so a write always matches the current camera.
    pub fn apply(&self, req: &SetRequest) -> Result<usize> {
        let fnumber_choices = parse_choices(&self.runner.get_config("f-number")?);
        let raw = self.read_raw().unwrap_or_default();
        let (params, _) = params_and_caps(&raw);
        let fps100 = params.fps100.unwrap_or(DEFAULT_FPS100);
        let writes = plan_writes(req, &fnumber_choices, fps100);
        let n = writes.len();
        for (key, value) in writes {
            self.runner.set_config(&key, &value)?;
        }
        Ok(n)
    }
}
