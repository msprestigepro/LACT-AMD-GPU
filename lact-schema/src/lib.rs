#[cfg(feature = "args")]
pub mod args;
mod profiles;
pub mod request;
mod response;

#[cfg(test)]
mod tests;

pub use request::Request;
pub use response::Response;

use amdgpu_sysfs::{
    gpu_handle::{
        fan_control::FanInfo,
        overdrive::{ClocksTable as _, ClocksTableGen as AmdClocksTableGen},
        PerformanceLevel,
    },
    hw_mon::Temperature,
};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::{self, Write},
    str::FromStr,
    sync::Arc,
};

pub const GIT_COMMIT: &str = env!("VERGEN_GIT_SHA");

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FanControlMode {
    Static,
    #[default]
    Curve,
}

impl FromStr for FanControlMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "curve" => Ok(Self::Curve),
            "static" => Ok(Self::Static),
            _ => Err("unknown fan control mode".to_string()),
        }
    }
}

pub type FanCurveMap = BTreeMap<i32, f32>;

pub fn default_fan_curve() -> FanCurveMap {
    [(40, 0.3), (50, 0.35), (60, 0.5), (70, 0.75), (80, 1.0)].into()
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Pong;

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug)]
pub struct SystemInfo {
    pub version: String,
    pub commit: Option<String>,
    pub profile: String,
    pub kernel_version: String,
    pub amdgpu_overdrive_enabled: Option<bool>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeviceListEntry {
    pub id: String,
    pub name: Option<String>,
}

impl fmt::Display for DeviceListEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => name.fmt(f),
            None => self.id.fmt(f),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GpuPciInfo {
    pub device_pci_info: PciInfo,
    pub subsystem_pci_info: PciInfo,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeviceInfo {
    pub pci_info: Option<GpuPciInfo>,
    pub vulkan_info: Option<VulkanInfo>,
    pub driver: String,
    pub vbios_version: Option<String>,
    pub link_info: LinkInfo,
    pub drm_info: Option<DrmInfo>,
}

impl DeviceInfo {
    pub fn vram_clock_ratio(&self) -> f64 {
        self.drm_info
            .as_ref()
            .map(|info| info.vram_clock_ratio)
            .unwrap_or(1.0)
    }

    pub fn info_elements(&self, stats: Option<&DeviceStats>) -> Vec<(&str, Option<String>)> {
        let pci_info = self.pci_info.as_ref();

        let mut gpu_model = self
            .drm_info
            .as_ref()
            .and_then(|drm| drm.device_name.as_deref())
            .or_else(|| pci_info.and_then(|pci_info| pci_info.device_pci_info.model.as_deref()))
            .unwrap_or("Unknown")
            .to_owned();

        let mut card_manufacturer = pci_info
            .and_then(|info| info.subsystem_pci_info.vendor.as_deref())
            .unwrap_or("Unknown")
            .to_owned();

        let mut card_model = pci_info
            .and_then(|info| info.subsystem_pci_info.model.as_deref())
            .unwrap_or("Unknown")
            .to_owned();

        if let Some(pci_info) = &self.pci_info {
            match self.drm_info {
                Some(DrmInfo {
                    pci_revision_id: Some(pci_rev),
                    ..
                }) => {
                    let _ = write!(
                        gpu_model,
                        " (0x{}:0x{}:0x{pci_rev:X})",
                        pci_info.device_pci_info.vendor_id, pci_info.device_pci_info.model_id,
                    );
                }
                _ => {
                    let _ = write!(
                        gpu_model,
                        " (0x{}:0x{})",
                        pci_info.device_pci_info.vendor_id, pci_info.device_pci_info.model_id
                    );
                }
            }

            let _ = write!(
                card_manufacturer,
                " (0x{})",
                pci_info.subsystem_pci_info.vendor_id
            );

            let _ = write!(card_model, " (0x{})", pci_info.subsystem_pci_info.model_id);
        };

        let mut elements = vec![
            ("GPU Model", Some(gpu_model)),
            ("Card Manufacturer", Some(card_manufacturer)),
            ("Card Model", Some(card_model)),
            ("Driver Used", Some(self.driver.clone())),
            ("VBIOS Version", self.vbios_version.clone()),
        ];

        if let Some(stats) = stats {
            elements.push((
                "VRAM Size",
                stats
                    .vram
                    .total
                    .map(|size| format!("{} MiB", size / 1024 / 1024)),
            ));
        }

        if let Some(drm_info) = &self.drm_info {
            elements.extend([
                ("GPU Family", drm_info.family_name.clone()),
                ("ASIC Name", drm_info.asic_name.clone()),
                (
                    "Compute Units",
                    drm_info.compute_units.map(|count| count.to_string()),
                ),
                (
                    "Execution Units",
                    drm_info
                        .intel
                        .execution_units
                        .map(|count| count.to_string()),
                ),
                (
                    "Subslices",
                    drm_info
                        .intel
                        .execution_units
                        .map(|count| count.to_string()),
                ),
                (
                    "Cuda Cores",
                    drm_info.cuda_cores.map(|count| count.to_string()),
                ),
                (
                    "SM Count",
                    drm_info
                        .streaming_multiprocessors
                        .map(|count| count.to_string()),
                ),
                (
                    "ROP Count",
                    drm_info.rop_info.as_ref().map(|rop| {
                        format!(
                            "{} ({} * {})",
                            rop.operations_count, rop.unit_count, rop.operations_factor
                        )
                    }),
                ),
                ("VRAM Type", drm_info.vram_type.clone()),
                ("VRAM Manufacturer", drm_info.vram_vendor.clone()),
                ("Theoretical VRAM Bandwidth", drm_info.vram_max_bw.clone()),
                (
                    "L1 Cache (Per CU)",
                    drm_info
                        .l1_cache_per_cu
                        .map(|cache| format!("{} KiB", cache / 1024)),
                ),
                (
                    "L2 Cache",
                    drm_info
                        .l2_cache
                        .map(|cache| format!("{} KiB", cache / 1024)),
                ),
                (
                    "L3 Cache",
                    drm_info.l3_cache_mb.map(|cache| format!("{cache} MiB")),
                ),
            ]);

            if let Some(memory_info) = &drm_info.memory_info {
                if let Some(rebar) = memory_info.resizeable_bar {
                    let rebar = if rebar { "Enabled" } else { "Disabled" };
                    elements.push(("Resizeable bar", Some(rebar.to_owned())));
                }

                elements.push((
                    "CPU Accessible VRAM",
                    Some((memory_info.cpu_accessible_total / 1024 / 1024).to_string()),
                ));
            }
        }

        if let (Some(link_speed), Some(link_width)) =
            (&self.link_info.current_speed, &self.link_info.current_width)
        {
            elements.push(("Link Speed", Some(format!("{link_speed} x{link_width}"))));
        }

        elements
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DrmInfo {
    pub device_name: Option<String>,
    pub pci_revision_id: Option<u32>,
    pub family_name: Option<String>,
    pub family_id: Option<u32>,
    pub asic_name: Option<String>,
    pub chip_class: Option<String>,
    pub compute_units: Option<u32>,
    pub streaming_multiprocessors: Option<u32>,
    pub cuda_cores: Option<u32>,
    pub vram_type: Option<String>,
    pub vram_vendor: Option<String>,
    pub vram_clock_ratio: f64,
    pub vram_bit_width: Option<u32>,
    pub vram_max_bw: Option<String>,
    pub l1_cache_per_cu: Option<u32>,
    pub l2_cache: Option<u32>,
    pub l3_cache_mb: Option<u32>,
    pub rop_info: Option<NvidiaRopInfo>,
    pub memory_info: Option<DrmMemoryInfo>,
    #[serde(flatten)]
    pub intel: IntelDrmInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NvidiaRopInfo {
    pub unit_count: u32,
    pub operations_factor: u32,
    pub operations_count: u32,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct IntelDrmInfo {
    pub execution_units: Option<u32>,
    pub subslices: Option<u32>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DrmMemoryInfo {
    pub cpu_accessible_used: u64,
    pub cpu_accessible_total: u64,
    pub resizeable_bar: Option<bool>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct ClocksInfo {
    pub max_sclk: Option<i32>,
    pub max_mclk: Option<i32>,
    pub max_voltage: Option<i32>,
    pub table: Option<ClocksTable>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ClocksTable {
    Amd(AmdClocksTableGen),
    Nvidia(NvidiaClocksTable),
    Intel(IntelClocksTable),
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct NvidiaClocksTable {
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub gpu_offsets: IndexMap<u32, NvidiaClockOffset>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub mem_offsets: IndexMap<u32, NvidiaClockOffset>,
    #[serde(default)]
    pub gpu_locked_clocks: Option<(u32, u32)>,
    #[serde(default)]
    pub vram_locked_clocks: Option<(u32, u32)>,
    #[serde(default)]
    pub gpu_clock_range: Option<(u32, u32)>,
    #[serde(default)]
    pub vram_clock_range: Option<(u32, u32)>,
}

/// Doc from `xe_gt_freq.c`
#[skip_serializing_none]
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
pub struct IntelClocksTable {
    pub gt_freq: Option<(u64, u64)>,
    /// - rpn_freq: The Render Performance (RP) N level, which is the minimal one.
    pub rpn_freq: Option<u64>,
    /// - rpe_freq: The Render Performance (RP) E level, which is the efficient one.
    pub rpe_freq: Option<u64>,
    /// - rp0_freq: The Render Performance (RP) 0 level, which is the maximum one.
    pub rp0_freq: Option<u64>,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct NvidiaClockOffset {
    pub current: i32,
    pub min: i32,
    pub max: i32,
}

impl From<AmdClocksTableGen> for ClocksInfo {
    fn from(table: AmdClocksTableGen) -> Self {
        let max_sclk = table.get_max_sclk();
        let max_mclk = table.get_max_mclk();
        let max_voltage = table.get_max_sclk_voltage();
        Self {
            max_sclk,
            max_mclk,
            max_voltage,
            table: Some(ClocksTable::Amd(table)),
        }
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LinkInfo {
    pub current_width: Option<String>,
    pub current_speed: Option<String>,
    pub max_width: Option<String>,
    pub max_speed: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct VulkanInfo {
    pub device_name: String,
    pub api_version: String,
    pub driver: VulkanDriverInfo,
    pub enabled_layers: Vec<String>,
    pub features: IndexMap<String, bool>,
    pub extensions: IndexMap<String, bool>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct VulkanDriverInfo {
    pub version: u32,
    pub name: Option<String>,
    pub info: Option<String>,
    pub driver_version: Option<String>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PciInfo {
    pub vendor_id: String,
    pub vendor: Option<String>,
    pub model_id: String,
    pub model: Option<String>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DeviceStats {
    pub fan: FanStats,
    pub clockspeed: ClockspeedStats,
    pub voltage: VoltageStats,
    pub vram: VramStats,
    pub power: PowerStats,
    pub temps: HashMap<String, Temperature>,
    pub busy_percent: Option<u8>,
    pub performance_level: Option<PerformanceLevel>,
    pub core_power_state: Option<usize>,
    pub memory_power_state: Option<usize>,
    pub pcie_power_state: Option<usize>,
    pub throttle_info: Option<BTreeMap<String, Vec<String>>>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FanStats {
    pub control_enabled: bool,
    pub control_mode: Option<FanControlMode>,
    pub static_speed: Option<f64>,
    pub curve: Option<FanCurveMap>,
    pub pwm_current: Option<u8>,
    pub speed_current: Option<u32>,
    pub speed_max: Option<u32>,
    pub speed_min: Option<u32>,
    pub spindown_delay_ms: Option<u64>,
    pub change_threshold: Option<u64>,
    // RDNA3+ params
    #[serde(default)]
    pub pmfw_info: PmfwInfo,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PmfwInfo {
    pub acoustic_limit: Option<FanInfo>,
    pub acoustic_target: Option<FanInfo>,
    pub target_temp: Option<FanInfo>,
    pub minimum_pwm: Option<FanInfo>,
    pub zero_rpm_enable: Option<bool>,
    pub zero_rpm_temperature: Option<FanInfo>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct ClockspeedStats {
    pub gpu_clockspeed: Option<u64>,
    /// Target clock
    pub current_gfxclk: Option<u64>,
    pub vram_clockspeed: Option<u64>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct VoltageStats {
    pub gpu: Option<u64>,
    pub northbridge: Option<u64>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct VramStats {
    pub total: Option<u64>,
    pub used: Option<u64>,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct PowerStats {
    pub average: Option<f64>,
    pub current: Option<f64>,
    pub cap_current: Option<f64>,
    pub cap_max: Option<f64>,
    pub cap_min: Option<f64>,
    pub cap_default: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PowerStates {
    pub core: Vec<PowerState>,
    pub vram: Vec<PowerState>,
}

impl PowerStates {
    pub fn is_empty(&self) -> bool {
        self.core.is_empty() && self.vram.is_empty()
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct PowerState {
    pub enabled: bool,
    pub min_value: Option<u64>,
    pub value: u64,
    pub index: Option<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitramfsType {
    Debian,
    Mkinitcpio,
    Dracut,
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PmfwOptions {
    pub acoustic_limit: Option<u32>,
    pub acoustic_target: Option<u32>,
    pub minimum_pwm: Option<u32>,
    pub target_temperature: Option<u32>,
    pub zero_rpm: Option<bool>,
    pub zero_rpm_threshold: Option<u32>,
}

impl PmfwOptions {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct FanOptions<'a> {
    pub id: &'a str,
    pub enabled: bool,
    pub mode: Option<FanControlMode>,
    pub static_speed: Option<f64>,
    pub curve: Option<FanCurveMap>,
    #[serde(default)]
    pub pmfw: PmfwOptions,
    pub spindown_delay_ms: Option<u64>,
    pub change_threshold: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct ProfilesInfo {
    pub profiles: IndexMap<String, Option<ProfileRule>>,
    pub current_profile: Option<String>,
    pub auto_switch: bool,
    pub watcher_state: Option<ProfileWatcherState>,
}

impl PartialEq for ProfilesInfo {
    fn eq(&self, other: &Self) -> bool {
        self.profiles.as_slice() == other.profiles.as_slice()
            && self.current_profile == other.current_profile
            && self.auto_switch == other.auto_switch
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", content = "filter", rename_all = "lowercase")]
pub enum ProfileRule {
    Process(ProcessProfileRule),
    Gamemode(Option<ProcessProfileRule>),
}

impl Default for ProfileRule {
    fn default() -> Self {
        Self::Process(ProcessProfileRule::default())
    }
}

#[skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProcessProfileRule {
    pub name: Arc<str>,
    pub args: Option<String>,
}

impl Default for ProcessProfileRule {
    fn default() -> Self {
        Self {
            name: String::new().into(),
            args: None,
        }
    }
}

pub type ProcessMap = IndexMap<i32, ProcessInfo>;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ProfileWatcherState {
    pub process_list: ProcessMap,
    pub gamemode_games: IndexSet<i32>,
    pub process_names_map: HashMap<Arc<str>, HashSet<i32>>,
}

#[allow(clippy::module_name_repetitions)]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProcessInfo {
    pub name: Arc<str>,
    pub cmdline: Box<str>,
}
