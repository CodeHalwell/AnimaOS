//! `anima doctor` — preflight hardware and provider detection (E9 S9.3).
//!
//! Detects GPU capabilities, reads RAM, probes local inference providers, checks
//! API-key configuration, and emits a structured [`DoctorReport`].  The report is
//! consumed directly by [`crate::init`] for the wizard path and printed in human-
//! readable form when the user runs `anima-hosted doctor`.
//!
//! All network probes use non-blocking TCP connect with a short timeout so the
//! command finishes quickly on a laptop with no local GPU servers running.

use std::io;
use std::io::Write as _;
use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::Duration;

// ── GPU detection ─────────────────────────────────────────────────────────────

/// Classification of the available compute device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuKind {
    /// NVIDIA GPU detected via `nvidia-smi`.
    Nvidia,
    /// Apple Silicon (M-series) on macOS.
    AppleSilicon,
    /// No GPU detected; CPU-only inference.
    CpuOnly,
}

/// Summary of the detected GPU (or CPU-only) compute surface.
#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub kind: GpuKind,
    /// Reported VRAM in GiB (rounded), or `None` for CPU-only.
    pub vram_gib: Option<u32>,
    /// Human-readable device name as reported by the driver.
    pub name: Option<String>,
}

impl GpuInfo {
    /// Returns a one-line summary suitable for the doctor report.
    pub fn summary(&self) -> String {
        match &self.kind {
            GpuKind::Nvidia => {
                let name = self.name.as_deref().unwrap_or("NVIDIA GPU");
                match self.vram_gib {
                    Some(v) => format!("{name} ({v} GiB VRAM)"),
                    None => name.to_string(),
                }
            }
            GpuKind::AppleSilicon => {
                let name = self.name.as_deref().unwrap_or("Apple Silicon");
                format!("{name} (unified memory)")
            }
            GpuKind::CpuOnly => "CPU-only (no discrete GPU detected)".to_string(),
        }
    }

    /// Returns `true` when local inference at the cheap-local tier is viable.
    pub fn can_run_local_inference(&self) -> bool {
        match self.kind {
            GpuKind::Nvidia => self.vram_gib.map(|v| v >= 4).unwrap_or(false),
            GpuKind::AppleSilicon => true, // unified memory; always viable
            GpuKind::CpuOnly => true,      // slow but possible
        }
    }
}

/// Detect the primary compute device.
///
/// Order of precedence:
/// 1. `nvidia-smi` — if present and succeeds, parse the first GPU.
/// 2. macOS `system_profiler SPHardwareDataType` — detect M-series.
/// 3. Fallback: CPU-only.
pub fn detect_gpu() -> GpuInfo {
    if let Some(info) = probe_nvidia_smi() {
        return info;
    }
    if let Some(info) = probe_apple_silicon() {
        return info;
    }
    GpuInfo {
        kind: GpuKind::CpuOnly,
        vram_gib: None,
        name: None,
    }
}

fn probe_nvidia_smi() -> Option<GpuInfo> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // First non-empty line: "GeForce RTX 3090, 24268"
    let first_line = stdout.lines().find(|l| !l.trim().is_empty())?;
    let mut parts = first_line.splitn(2, ',');
    let name = parts.next()?.trim().to_string();
    let vram_mib: u32 = parts.next()?.trim().parse().ok()?;
    let vram_gib = (vram_mib + 512) / 1024; // round to nearest GiB
    Some(GpuInfo {
        kind: GpuKind::Nvidia,
        vram_gib: Some(vram_gib),
        name: Some(name),
    })
}

fn probe_apple_silicon() -> Option<GpuInfo> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()?;
    let brand = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if brand.contains("Apple M") {
        Some(GpuInfo {
            kind: GpuKind::AppleSilicon,
            vram_gib: None,
            name: Some(brand),
        })
    } else {
        None
    }
}

// ── RAM detection ─────────────────────────────────────────────────────────────

/// Total RAM in GiB (rounded), or `None` if detection failed.
pub fn detect_ram_gib() -> Option<u32> {
    detect_ram_gib_platform()
}

#[cfg(target_os = "linux")]
fn detect_ram_gib_platform() -> Option<u32> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    // MemTotal:       16384000 kB
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(((kb + (512 * 1024)) / (1024 * 1024)) as u32) // round to nearest GiB
}

#[cfg(not(target_os = "linux"))]
fn detect_ram_gib_platform() -> Option<u32> {
    // macOS / other: use sysctl
    let output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    let bytes: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    Some(((bytes + (512 * 1024 * 1024)) / (1024 * 1024 * 1024)) as u32)
}

// ── Provider detection ────────────────────────────────────────────────────────

/// Status of a detected or configured LLM provider.
#[derive(Debug, Clone)]
pub struct ProviderStatus {
    /// Short name, e.g. `"ollama"`.
    pub name: &'static str,
    /// The URL that was probed, e.g. `"http://localhost:11434"`.
    pub url: &'static str,
    /// TCP connect succeeded — server is listening.
    pub reachable: bool,
    /// Relevant env var is set (API key or custom URL), regardless of reachability.
    pub configured: bool,
    /// Recommended tier if this provider is the best available.
    pub tier: &'static str,
}

impl ProviderStatus {
    /// Returns a one-line status string for the doctor report.
    pub fn status_line(&self) -> String {
        let reach = if self.reachable {
            "✅ REACHABLE  "
        } else {
            "❌ NOT FOUND  "
        };
        let cfg = if self.configured {
            " [env configured]"
        } else {
            ""
        };
        format!("{reach}{}  tier={}{cfg}", self.url, self.tier)
    }
}

/// Known local-inference endpoints to probe.
static PROVIDER_PROBES: &[(&str, &str, Option<&str>, &str)] = &[
    // (name, host:port, env_var_for_configured_hint, tier)
    (
        "ollama",
        "127.0.0.1:11434",
        Some("ANIMA_OLLAMA_URL"),
        "cheap-local",
    ),
    (
        "lmstudio",
        "127.0.0.1:1234",
        Some("ANIMA_LMSTUDIO_URL"),
        "mid-tier",
    ),
    ("vllm", "127.0.0.1:8000", Some("ANIMA_VLLM_URL"), "frontier"),
    (
        "llamacpp-server",
        "127.0.0.1:8080",
        Some("ANIMA_LLAMACPP_URL"),
        "mid-tier",
    ),
];

/// Known hosted-API providers (no TCP probe — check env key only).
static API_PROVIDERS: &[(&str, &str, &str)] = &[
    ("anthropic", "ANTHROPIC_API_KEY", "frontier"),
    ("openai", "OPENAI_API_KEY", "frontier"),
];

/// Probe all known local providers and API key env vars.
pub fn detect_providers() -> Vec<ProviderStatus> {
    let probe_timeout = Duration::from_millis(500);
    let mut results = Vec::new();

    // Local servers: TCP connect probe
    for &(name, addr, env_key, tier) in PROVIDER_PROBES {
        let reachable = addr
            .parse::<SocketAddr>()
            .ok()
            .map(|sa| TcpStream::connect_timeout(&sa, probe_timeout).is_ok())
            .unwrap_or(false);
        let configured = env_key
            .map(|k| std::env::var(k).map(|v| !v.is_empty()).unwrap_or(false))
            .unwrap_or(false);
        results.push(ProviderStatus {
            name,
            url: addr,
            reachable,
            configured,
            tier,
        });
    }

    // Hosted APIs: env-key check only
    for &(name, key, tier) in API_PROVIDERS {
        let configured = std::env::var(key).map(|v| !v.is_empty()).unwrap_or(false);
        results.push(ProviderStatus {
            name,
            url: if name == "anthropic" {
                "api.anthropic.com"
            } else {
                "api.openai.com"
            },
            reachable: false, // not probed
            configured,
            tier,
        });
    }

    results
}

// ── Recommendation ────────────────────────────────────────────────────────────

/// Recommended backend binding for the three router tiers.
#[derive(Debug, Clone)]
pub struct TierRecommendation {
    pub cheap_local: String,
    pub mid_tier: String,
    pub frontier: String,
    pub notes: Vec<String>,
}

/// Derive tier recommendations from the detected hardware and providers.
pub fn recommend(gpu: &GpuInfo, providers: &[ProviderStatus]) -> TierRecommendation {
    let mut notes = Vec::new();

    // Best available local provider: prefer ollama → lmstudio/llamacpp → cpu
    let local_provider = providers
        .iter()
        .find(|p| p.reachable && (p.name == "ollama" || p.name == "llamacpp-server"))
        .or_else(|| {
            providers
                .iter()
                .find(|p| p.reachable && p.name == "lmstudio")
        });

    // Best hosted API provider: prefer anthropic
    let api_provider = providers
        .iter()
        .find(|p| p.configured && p.name == "anthropic")
        .or_else(|| {
            providers
                .iter()
                .find(|p| p.configured && p.name == "openai")
        });

    let cheap_local = if let Some(p) = local_provider {
        match p.name {
            "ollama" => "ollama (GGUF via Ollama; run: `ollama pull llama3.2:3b`)".to_string(),
            "llamacpp-server" => "llamacpp-server".to_string(),
            _ => p.name.to_string(),
        }
    } else {
        notes.push(
            "No local inference server detected. Install Ollama (https://ollama.com) \
             for cheap-local inference."
                .to_string(),
        );
        "mock (no local provider; install Ollama)".to_string()
    };

    let (mid_tier, frontier) = if let Some(p) = api_provider {
        let api = p.name.to_string();
        (
            local_provider
                .map(|lp| format!("{} (or {})", lp.name, api))
                .unwrap_or_else(|| api.clone()),
            api,
        )
    } else {
        notes.push(
            "No hosted-API key found. Set ANTHROPIC_API_KEY or OPENAI_API_KEY \
             for frontier routing."
                .to_string(),
        );
        let fallback = local_provider
            .map(|p| p.name.to_string())
            .unwrap_or_else(|| "mock".to_string());
        (fallback.clone(), fallback)
    };

    if !gpu.can_run_local_inference() {
        notes.push("Limited GPU VRAM: consider quantised models (Q4_K_M or smaller).".to_string());
    }

    TierRecommendation {
        cheap_local,
        mid_tier,
        frontier,
        notes,
    }
}

// ── Full report ───────────────────────────────────────────────────────────────

/// Complete preflight report emitted by `anima doctor`.
#[derive(Debug)]
pub struct DoctorReport {
    pub gpu: GpuInfo,
    pub ram_gib: Option<u32>,
    pub providers: Vec<ProviderStatus>,
    pub recommendation: TierRecommendation,
}

/// Run the full preflight suite and return a [`DoctorReport`].
pub fn run_doctor() -> DoctorReport {
    let gpu = detect_gpu();
    let ram_gib = detect_ram_gib();
    let providers = detect_providers();
    let recommendation = recommend(&gpu, &providers);
    DoctorReport {
        gpu,
        ram_gib,
        providers,
        recommendation,
    }
}

/// Print the doctor report to stdout in a human-readable format.
pub fn print_report(report: &DoctorReport) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    writeln!(out, "\nanima doctor — system preflight (E9 S9.3)\n").ok();

    // Hardware section
    writeln!(out, "━━━ Hardware").ok();
    writeln!(out, "  GPU  : {}", report.gpu.summary()).ok();
    match report.ram_gib {
        Some(gb) => writeln!(out, "  RAM  : ~{gb} GiB").ok(),
        None => writeln!(out, "  RAM  : (detection unavailable on this platform)").ok(),
    };
    writeln!(out).ok();

    // Providers section
    writeln!(out, "━━━ Local providers").ok();
    for p in &report.providers {
        writeln!(out, "  {:<18} {}", p.name, p.status_line()).ok();
    }
    writeln!(out).ok();

    // Recommendation section
    let rec = &report.recommendation;
    writeln!(out, "━━━ Recommendation").ok();
    writeln!(out, "  cheap-local  → {}", rec.cheap_local).ok();
    writeln!(out, "  mid-tier     → {}", rec.mid_tier).ok();
    writeln!(out, "  frontier     → {}", rec.frontier).ok();
    if !rec.notes.is_empty() {
        writeln!(out).ok();
        for note in &rec.notes {
            writeln!(out, "  ⚠  {note}").ok();
        }
    }
    writeln!(out, "\nRun `anima-hosted init` to set up your agent.\n").ok();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── GpuInfo ──────────────────────────────────────────────────────────────

    #[test]
    fn gpu_info_nvidia_summary_includes_vram() {
        let info = GpuInfo {
            kind: GpuKind::Nvidia,
            vram_gib: Some(24),
            name: Some("RTX 3090".to_string()),
        };
        assert!(info.summary().contains("RTX 3090"));
        assert!(info.summary().contains("24 GiB"));
    }

    #[test]
    fn gpu_info_apple_silicon_summary() {
        let info = GpuInfo {
            kind: GpuKind::AppleSilicon,
            vram_gib: None,
            name: Some("Apple M2 Pro".to_string()),
        };
        assert!(info.summary().contains("Apple M2 Pro"));
        assert!(info.summary().contains("unified"));
    }

    #[test]
    fn gpu_info_cpu_only_summary() {
        let info = GpuInfo {
            kind: GpuKind::CpuOnly,
            vram_gib: None,
            name: None,
        };
        assert!(info.summary().contains("CPU-only"));
    }

    #[test]
    fn nvidia_4gib_can_run_inference() {
        let info = GpuInfo {
            kind: GpuKind::Nvidia,
            vram_gib: Some(4),
            name: None,
        };
        assert!(info.can_run_local_inference());
    }

    #[test]
    fn nvidia_2gib_cannot_run_inference() {
        let info = GpuInfo {
            kind: GpuKind::Nvidia,
            vram_gib: Some(2),
            name: None,
        };
        assert!(!info.can_run_local_inference());
    }

    #[test]
    fn apple_silicon_can_always_run_inference() {
        let info = GpuInfo {
            kind: GpuKind::AppleSilicon,
            vram_gib: None,
            name: None,
        };
        assert!(info.can_run_local_inference());
    }

    #[test]
    fn cpu_only_can_run_inference_slowly() {
        let info = GpuInfo {
            kind: GpuKind::CpuOnly,
            vram_gib: None,
            name: None,
        };
        assert!(info.can_run_local_inference());
    }

    // ── ProviderStatus ────────────────────────────────────────────────────────

    #[test]
    fn provider_status_reachable_shows_green_tick() {
        let p = ProviderStatus {
            name: "ollama",
            url: "127.0.0.1:11434",
            reachable: true,
            configured: false,
            tier: "cheap-local",
        };
        let line = p.status_line();
        assert!(line.contains('✅'));
        assert!(line.contains("cheap-local"));
    }

    #[test]
    fn provider_status_unreachable_shows_red_cross() {
        let p = ProviderStatus {
            name: "vllm",
            url: "127.0.0.1:8000",
            reachable: false,
            configured: false,
            tier: "frontier",
        };
        let line = p.status_line();
        assert!(line.contains('❌'));
        assert!(line.contains("frontier"));
    }

    #[test]
    fn provider_status_configured_shows_env_hint() {
        let p = ProviderStatus {
            name: "ollama",
            url: "127.0.0.1:11434",
            reachable: false,
            configured: true,
            tier: "cheap-local",
        };
        assert!(p.status_line().contains("env configured"));
    }

    // ── TierRecommendation ────────────────────────────────────────────────────

    #[test]
    fn recommend_with_ollama_and_anthropic_key() {
        let gpu = GpuInfo {
            kind: GpuKind::Nvidia,
            vram_gib: Some(24),
            name: None,
        };
        let providers = vec![
            ProviderStatus {
                name: "ollama",
                url: "127.0.0.1:11434",
                reachable: true,
                configured: false,
                tier: "cheap-local",
            },
            ProviderStatus {
                name: "anthropic",
                url: "api.anthropic.com",
                reachable: false,
                configured: true,
                tier: "frontier",
            },
        ];
        let rec = recommend(&gpu, &providers);
        assert!(rec.cheap_local.contains("ollama"));
        assert!(rec.frontier.contains("anthropic"));
        assert!(rec.notes.is_empty());
    }

    #[test]
    fn recommend_no_providers_adds_notes() {
        let gpu = GpuInfo {
            kind: GpuKind::CpuOnly,
            vram_gib: None,
            name: None,
        };
        let providers: Vec<ProviderStatus> = vec![];
        let rec = recommend(&gpu, &providers);
        assert!(rec.cheap_local.contains("mock"));
        assert!(!rec.notes.is_empty());
    }

    #[test]
    fn recommend_low_vram_adds_quantisation_note() {
        let gpu = GpuInfo {
            kind: GpuKind::Nvidia,
            vram_gib: Some(2),
            name: None,
        };
        let providers: Vec<ProviderStatus> = vec![];
        let rec = recommend(&gpu, &providers);
        assert!(rec.notes.iter().any(|n| n.contains("VRAM")));
    }

    // ── nvidia-smi parsing helper ─────────────────────────────────────────────

    #[test]
    fn nvidia_smi_output_parses_name_and_vram() {
        // Simulate what probe_nvidia_smi would produce from the command output.
        let line = "GeForce RTX 3090, 24268";
        let mut parts = line.splitn(2, ',');
        let name = parts.next().unwrap().trim().to_string();
        let vram_mib: u32 = parts.next().unwrap().trim().parse().unwrap();
        let vram_gib = (vram_mib + 512) / 1024;

        assert_eq!(name, "GeForce RTX 3090");
        assert_eq!(vram_gib, 24);
    }

    #[test]
    fn nvidia_smi_vram_rounds_correctly() {
        // 8116 MiB should round to 8 GiB.
        let vram_mib: u32 = 8116;
        let vram_gib = (vram_mib + 512) / 1024;
        assert_eq!(vram_gib, 8);

        // 15360 MiB (16 GiB exactly) should stay 15.
        let vram_mib: u32 = 15360;
        let vram_gib = (vram_mib + 512) / 1024;
        assert_eq!(vram_gib, 15);
    }
}
