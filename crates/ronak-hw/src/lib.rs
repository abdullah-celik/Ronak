//! # ronak-hw
//!
//! Finds out what the machine can run models on, and plans where each of
//! Ronak's two inference tiers should go.
//!
//! On Linux everything comes from two virtual filesystems the kernel exposes:
//!
//! - `/proc` describes processes and the system (`/proc/cpuinfo`, `/proc/meminfo`).
//! - `/sys` describes devices. NPUs appear under `/sys/class/accel/accelN`
//!   (the kernel's "compute accelerator" subsystem, which also creates
//!   `/dev/accel/accelN`), GPUs under `/sys/class/drm/cardN`. Each device
//!   links to its PCI vendor and device ids and to the driver bound to it.
//!
//! Reading these needs no special permissions. All paths are resolved under a
//! root directory so tests can build a fake `/sys` in a temp folder.
//! Windows support will read the same facts from DXCore and WMI.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    Amd,
    Intel,
    Nvidia,
    Qualcomm,
    Other,
}

impl Vendor {
    fn from_pci(id: &str) -> Self {
        match id.trim().to_ascii_lowercase().as_str() {
            "0x1022" | "0x1002" => Vendor::Amd,
            "0x8086" => Vendor::Intel,
            "0x10de" => Vendor::Nvidia,
            "0x17cb" | "0x5143" => Vendor::Qualcomm,
            _ => Vendor::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Cpu {
    pub model: String,
    pub logical_cores: usize,
    /// Instruction-set features relevant to inference speed.
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Npu {
    pub vendor: Vendor,
    pub driver: String,
    /// e.g. /dev/accel/accel0
    pub device_node: String,
    /// "vendor:device", e.g. "1022:17f0".
    pub pci_id: Option<String>,
    /// Human-readable family, e.g. "AMD XDNA 2 (Strix / Krackan / Strix Halo)".
    pub family: String,
    /// Whether a known Linux runtime can run LLMs on it today.
    pub linux_llm_runtime: Option<String>,
    /// Whether ONNX Runtime has an execution provider for it (tier-1 classifier).
    pub onnx_runtime_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Gpu {
    pub vendor: Vendor,
    pub driver: String,
    pub pci_id: Option<String>,
    pub vram_mb: Option<u64>,
    pub integrated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct HardwareProfile {
    pub os: String,
    pub kernel: Option<String>,
    pub cpu: Cpu,
    pub memory_mb: u64,
    pub npus: Vec<Npu>,
    pub gpus: Vec<Gpu>,
}

/// A local inference server found on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalServer {
    /// e.g. "ollama", "lemonade", "llama.cpp".
    pub kind: String,
    /// OpenAI-compatible base URL ending in /v1.
    pub base_url: String,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Device {
    Npu,
    Gpu,
    Cpu,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierPlan {
    pub device: Device,
    pub runtime: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InferencePlan {
    /// Tier 1: a tiny always-on task classifier.
    pub classifier: TierPlan,
    /// Tier 2: an occasional local LLM call when the classifier is unsure.
    pub llm: TierPlan,
    /// Suggested LLM size for this much memory.
    pub llm_model_hint: String,
    /// The server to use for tier 2, if one is running.
    pub llm_server: Option<LocalServer>,
    pub notes: Vec<String>,
}

/// Probes the real machine.
pub fn probe() -> HardwareProfile {
    probe_root(Path::new("/"))
}

/// Probes with every path resolved under `root` (for tests).
pub fn probe_root(root: &Path) -> HardwareProfile {
    let cpuinfo = read(root, "proc/cpuinfo").unwrap_or_default();
    let meminfo = read(root, "proc/meminfo").unwrap_or_default();
    HardwareProfile {
        os: std::env::consts::OS.to_string(),
        kernel: read(root, "proc/sys/kernel/osrelease").map(|s| s.trim().to_string()),
        cpu: parse_cpu(&cpuinfo),
        memory_mb: parse_mem_kb(&meminfo) / 1024,
        npus: probe_npus(root),
        gpus: probe_gpus(root),
    }
}

fn read(root: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(root.join(rel)).ok()
}

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn driver_name(device_dir: &Path) -> String {
    fs::read_link(device_dir.join("driver"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".into())
}

fn pci_id(device_dir: &Path) -> (Option<String>, Option<String>) {
    let vendor = read_trim(&device_dir.join("vendor"));
    let device = read_trim(&device_dir.join("device"));
    let id = match (&vendor, &device) {
        (Some(v), Some(d)) => Some(format!("{}:{}", v.trim_start_matches("0x"), d.trim_start_matches("0x"))),
        _ => None,
    };
    (vendor, id)
}

fn parse_cpu(cpuinfo: &str) -> Cpu {
    let field = |name: &str| {
        cpuinfo
            .lines()
            .find(|l| l.split(':').next().is_some_and(|k| k.trim() == name))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_string())
    };
    let interesting = ["avx2", "avx512f", "avx512_vnni", "avx_vnni", "amx_tile", "amx_int8", "fma", "f16c"];
    let flags = field("flags").or_else(|| field("Features")).unwrap_or_default();
    let features =
        interesting.iter().filter(|f| flags.split_whitespace().any(|x| x == **f)).map(|f| f.to_string()).collect();
    let logical_cores = cpuinfo.lines().filter(|l| l.starts_with("processor")).count().max(1);
    Cpu { model: field("model name").unwrap_or_else(|| "unknown CPU".into()), logical_cores, features }
}

fn parse_mem_kb(meminfo: &str) -> u64 {
    meminfo
        .lines()
        .find(|l| l.starts_with("MemTotal:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> =
        fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    entries.sort();
    entries
}

fn probe_npus(root: &Path) -> Vec<Npu> {
    sorted_entries(&root.join("sys/class/accel"))
        .into_iter()
        .filter_map(|entry| {
            let name = entry.file_name()?.to_string_lossy().into_owned();
            if !name.starts_with("accel") {
                return None;
            }
            let dev = entry.join("device");
            let driver = driver_name(&dev);
            let (vendor_raw, pci) = pci_id(&dev);
            let vendor = vendor_raw.as_deref().map(Vendor::from_pci).unwrap_or(Vendor::Other);
            let device_id = pci.as_deref().and_then(|p| p.split(':').nth(1)).unwrap_or("").to_string();
            let (family, llm, onnx) = classify_npu(&driver, &device_id);
            Some(Npu {
                vendor,
                driver,
                device_node: format!("/dev/accel/{name}"),
                pci_id: pci,
                family,
                linux_llm_runtime: llm.map(str::to_owned),
                onnx_runtime_path: onnx.map(str::to_owned),
            })
        })
        .collect()
}

/// Known NPU families. Device ids are best effort; unknown ones still show
/// up with their driver name.
fn classify_npu(driver: &str, device_id: &str) -> (String, Option<&'static str>, Option<&'static str>) {
    match (driver, device_id) {
        ("amdxdna", "1502") => (
            "AMD XDNA, first generation (Phoenix / Hawk Point)".into(),
            None,
            Some("ONNX Runtime with the Vitis AI execution provider (Windows-first; Linux experimental)"),
        ),
        ("amdxdna", "17f0") => (
            "AMD XDNA 2 (Strix / Krackan / Strix Halo)".into(),
            Some("FastFlowLM, usually through Lemonade Server"),
            Some("ONNX Runtime with the Vitis AI execution provider (experimental on Linux)"),
        ),
        ("amdxdna", _) => ("AMD XDNA (unrecognized generation)".into(), None, None),
        ("intel_vpu", id) => {
            let generation = match id {
                "7d1d" => "Meteor Lake",
                "ad1d" => "Arrow Lake",
                "643e" => "Lunar Lake",
                _ => "unrecognized generation",
            };
            (
                format!("Intel NPU ({generation})"),
                Some("OpenVINO GenAI or OpenVINO Model Server"),
                Some("ONNX Runtime with the OpenVINO execution provider (device NPU)"),
            )
        }
        (other, _) => (format!("accelerator using driver '{other}'"), None, None),
    }
}

fn probe_gpus(root: &Path) -> Vec<Gpu> {
    sorted_entries(&root.join("sys/class/drm"))
        .into_iter()
        .filter_map(|entry| {
            let name = entry.file_name()?.to_string_lossy().into_owned();
            // "card0" is a GPU; "card0-eDP-1" is one of its connectors.
            let is_card =
                name.strip_prefix("card").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
            if !is_card {
                return None;
            }
            let dev = entry.join("device");
            let driver = driver_name(&dev);
            let (vendor_raw, pci) = pci_id(&dev);
            let vendor = vendor_raw.as_deref().map(Vendor::from_pci).unwrap_or(Vendor::Other);
            let vram_mb = read_trim(&dev.join("mem_info_vram_total"))
                .and_then(|b| b.parse::<u64>().ok())
                .map(|b| b / (1024 * 1024));
            let integrated = match vendor {
                Vendor::Intel => driver == "i915" || driver == "xe",
                // APUs get a small BIOS carve-out; discrete Radeons have far more.
                Vendor::Amd => vram_mb.is_none_or(|v| v <= 4096),
                _ => false,
            };
            Some(Gpu { vendor, driver, pci_id: pci, vram_mb, integrated })
        })
        .collect()
}

/// Decides where each tier should run, given the hardware and any local
/// inference servers that are already running.
pub fn plan(hw: &HardwareProfile, servers: &[LocalServer]) -> InferencePlan {
    let mut notes = Vec::new();
    let intel_npu = hw.npus.iter().find(|n| n.driver == "intel_vpu");
    let xdna2 = hw.npus.iter().find(|n| n.driver == "amdxdna" && n.linux_llm_runtime.is_some());
    let xdna1 = hw.npus.iter().find(|n| n.driver == "amdxdna" && n.linux_llm_runtime.is_none());
    let big_nvidia = hw.gpus.iter().find(|g| g.vendor == Vendor::Nvidia);
    let big_amd = hw.gpus.iter().find(|g| g.vendor == Vendor::Amd && !g.integrated && g.vram_mb.unwrap_or(0) >= 8192);
    let igpu = hw.gpus.iter().find(|g| g.integrated);
    let server = |kind: &str| servers.iter().find(|s| s.kind == kind).cloned();

    // Tier 1: tiny, always on, so power matters more than speed.
    let classifier = if let Some(n) = intel_npu {
        TierPlan {
            device: Device::Npu,
            runtime: n.onnx_runtime_path.clone().unwrap_or_default(),
            reason: "always-on work belongs on the most power-efficient unit".into(),
        }
    } else if let Some(n) = xdna2 {
        TierPlan {
            device: Device::Npu,
            runtime: n.onnx_runtime_path.clone().unwrap_or_default(),
            reason:
                "always-on work belongs on the most power-efficient unit (Linux support is young; falls back to CPU)"
                    .into(),
        }
    } else {
        TierPlan {
            device: Device::Cpu,
            runtime: "ONNX Runtime (CPU)".into(),
            reason: "the classifier is tiny; a CPU core handles it in well under a millisecond".into(),
        }
    };

    // Tier 2: occasional, so speed matters more than power.
    let (llm, llm_server) = if let (Some(_), Some(s)) = (xdna2, server("lemonade")) {
        (
            TierPlan {
                device: Device::Npu,
                runtime: "Lemonade Server with FastFlowLM".into(),
                reason: "XDNA 2 NPU with a running Lemonade server".into(),
            },
            Some(s),
        )
    } else if let Some(g) = big_nvidia {
        let reason = match g.vram_mb {
            Some(v) => format!("NVIDIA GPU with {v} MB VRAM"),
            None => "NVIDIA GPU".into(),
        };
        (
            TierPlan { device: Device::Gpu, runtime: "Ollama or llama.cpp (CUDA)".into(), reason },
            server("ollama").or_else(|| server("llama.cpp")),
        )
    } else if big_amd.is_some() {
        (
            TierPlan {
                device: Device::Gpu,
                runtime: "Ollama (ROCm) or llama.cpp (Vulkan)".into(),
                reason: "discrete AMD GPU".into(),
            },
            server("ollama").or_else(|| server("llama.cpp")),
        )
    } else if intel_npu.is_some() {
        (
            TierPlan {
                device: Device::Npu,
                runtime: "OpenVINO GenAI / OpenVINO Model Server".into(),
                reason: "Intel NPU".into(),
            },
            server("openvino"),
        )
    } else if xdna2.is_some() {
        notes.push("Your AMD XDNA 2 NPU can run LLMs on Linux via Lemonade Server (FastFlowLM). Start it and Ronak will use it.".into());
        (
            TierPlan {
                device: Device::Cpu,
                runtime: "Ollama or llama.cpp (CPU) until Lemonade is running".into(),
                reason: "XDNA 2 NPU present but no Lemonade server found".into(),
            },
            server("ollama").or_else(|| server("llama.cpp")),
        )
    } else if igpu.is_some() {
        (
            TierPlan {
                device: Device::Gpu,
                runtime: "llama.cpp (Vulkan) or Ollama".into(),
                reason: "integrated GPU; fine for small models".into(),
            },
            server("llama.cpp").or_else(|| server("ollama")),
        )
    } else {
        (
            TierPlan {
                device: Device::Cpu,
                runtime: "Ollama or llama.cpp (CPU)".into(),
                reason: "no accelerator found".into(),
            },
            server("ollama").or_else(|| server("llama.cpp")),
        )
    };

    if xdna1.is_some() {
        notes.push("First-generation XDNA NPU detected. No mainstream Linux runtime runs LLMs on it yet, so Ronak uses the CPU or iGPU instead.".into());
    }
    if hw.npus.is_empty() {
        notes.push("No NPU found under /sys/class/accel. If your CPU has one, a newer kernel or driver may be needed (amdxdna for AMD, intel_vpu for Intel).".into());
    }
    if servers.is_empty() {
        notes.push("No local LLM server is running, so tier 2 stays off. Any OpenAI-compatible local server works (Ollama, Lemonade, llama.cpp server).".into());
    }

    let gb = hw.memory_mb / 1024;
    let llm_model_hint = match gb {
        g if g >= 30 => "7-8B parameters, 4-bit quantized",
        g if g >= 15 => "3-4B parameters, 4-bit quantized",
        _ => "1-1.5B parameters, 4-bit quantized",
    }
    .to_string();

    InferencePlan { classifier, llm, llm_model_hint, llm_server, notes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn device(root: &Path, class_entry: &str, driver: &str, vendor: &str, dev: &str) {
        let d = root.join(class_entry).join("device");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("vendor"), format!("{vendor}\n")).unwrap();
        fs::write(d.join("device"), format!("{dev}\n")).unwrap();
        symlink(format!("../../../bus/pci/drivers/{driver}"), d.join("driver")).unwrap();
    }

    /// A fake Ryzen AI 300 laptop: XDNA 2 NPU plus an integrated Radeon.
    fn strix_laptop() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        write(
            r,
            "proc/cpuinfo",
            "processor\t: 0\nmodel name\t: AMD Ryzen AI 9 365\nflags\t\t: fpu avx2 avx512f fma\n\nprocessor\t: 1\nmodel name\t: AMD Ryzen AI 9 365\n",
        );
        write(r, "proc/meminfo", "MemTotal:       32562320 kB\n");
        write(r, "proc/sys/kernel/osrelease", "6.17.0-generic\n");
        device(r, "sys/class/accel/accel0", "amdxdna", "0x1022", "0x17f0");
        device(r, "sys/class/drm/card1", "amdgpu", "0x1002", "0x150e");
        write(r, "sys/class/drm/card1/device/mem_info_vram_total", "536870912\n");
        fs::create_dir_all(r.join("sys/class/drm/card1-eDP-1")).unwrap();
        tmp
    }

    #[test]
    fn detects_xdna2_and_igpu() {
        let tmp = strix_laptop();
        let hw = probe_root(tmp.path());
        assert_eq!(hw.cpu.logical_cores, 2);
        assert_eq!(hw.cpu.features, ["avx2", "avx512f", "fma"]);
        assert_eq!(hw.memory_mb, 31799);
        assert_eq!(hw.npus.len(), 1);
        assert_eq!(hw.npus[0].pci_id.as_deref(), Some("1022:17f0"));
        assert!(hw.npus[0].family.contains("XDNA 2"));
        assert_eq!(hw.gpus.len(), 1, "connectors must not count as GPUs");
        assert!(hw.gpus[0].integrated);
        assert_eq!(hw.gpus[0].vram_mb, Some(512));
    }

    #[test]
    fn plan_prefers_npu_when_a_server_runs() {
        let tmp = strix_laptop();
        let hw = probe_root(tmp.path());
        let without = plan(&hw, &[]);
        assert_eq!(without.classifier.device, Device::Npu);
        assert_eq!(without.llm.device, Device::Cpu);
        assert!(without.notes.iter().any(|n| n.contains("Lemonade")));

        let lemonade =
            LocalServer { kind: "lemonade".into(), base_url: "http://127.0.0.1:8000/api/v1".into(), models: vec![] };
        let with = plan(&hw, std::slice::from_ref(&lemonade));
        assert_eq!(with.llm.device, Device::Npu);
        assert_eq!(with.llm_server, Some(lemonade));
        assert_eq!(with.llm_model_hint, "7-8B parameters, 4-bit quantized");
    }

    #[test]
    fn empty_machine_falls_back_to_cpu() {
        let tmp = tempfile::tempdir().unwrap();
        let p = plan(&probe_root(tmp.path()), &[]);
        assert_eq!(p.classifier.device, Device::Cpu);
        assert_eq!(p.llm.device, Device::Cpu);
        assert!(serde_json::to_string(&p).is_ok());
    }
}
