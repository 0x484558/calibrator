use std::collections::HashSet;
use std::mem::{align_of, size_of};

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID};

const DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE =
    DISPLAYCONFIG_DEVICE_INFO_TYPE(15);
const DISPLAYCONFIG_DEVICE_INFO_SET_SDR_WHITE_LEVEL: DISPLAYCONFIG_DEVICE_INFO_TYPE =
    DISPLAYCONFIG_DEVICE_INFO_TYPE(0xffff_ffee_u32.cast_signed());
const HDR_MODE: i32 = 2;
const MAX_TOPOLOGY_SNAPSHOTS: usize = 3;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigGetAdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    flags: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigSetSdrWhiteLevel {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    sdr_white_level: u32,
    final_value: u8,
}

const _: () = assert!(size_of::<DISPLAYCONFIG_DEVICE_INFO_HEADER>() == 20);
const _: () = assert!(size_of::<DisplayConfigSetSdrWhiteLevel>() == 28);
const _: () = assert!(align_of::<DisplayConfigSetSdrWhiteLevel>() == 4);
const _: () = assert!(size_of::<DisplayConfigGetAdvancedColorInfo2>() == 36);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TargetKey {
    adapter_low: u32,
    adapter_high: i32,
    target_id: u32,
}

impl TargetKey {
    fn new(adapter: LUID, target_id: u32) -> Self {
        Self {
            adapter_low: adapter.LowPart,
            adapter_high: adapter.HighPart,
            target_id,
        }
    }
}

#[derive(Clone, Copy)]
struct HdrTarget {
    adapter: LUID,
    target_id: u32,
    current_white_level: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProbeSample {
    pub(crate) adapter_low: u32,
    pub(crate) adapter_high: i32,
    pub(crate) target_id: u32,
    pub(crate) raw_white_level: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProbeOutcome {
    Sample(ProbeSample),
    NoHdrTarget,
    AmbiguousInternalTargets,
    TopologyUnavailable,
    InspectionUnavailable,
}

pub(crate) enum ApplyOutcome {
    AppliedAndVerified,
    AlreadyCorrect,
    NoHdrTarget,
    AmbiguousInternalTargets,
    TopologyUnavailable,
    InspectionUnavailable,
    SetFailed(i32),
    VerificationFailed {
        expected: u32,
        observed: Option<u32>,
    },
}

pub(crate) fn apply_sdr_white_level(required_white_level: u32) -> ApplyOutcome {
    let target = match locate_hdr_target() {
        TargetLookup::Found(target) => target,
        TargetLookup::NoHdrTarget => return ApplyOutcome::NoHdrTarget,
        TargetLookup::AmbiguousInternalTargets => {
            return ApplyOutcome::AmbiguousInternalTargets;
        }
        TargetLookup::TopologyUnavailable => return ApplyOutcome::TopologyUnavailable,
        TargetLookup::InspectionUnavailable => return ApplyOutcome::InspectionUnavailable,
    };

    if target.current_white_level == required_white_level {
        return ApplyOutcome::AlreadyCorrect;
    }

    // SAFETY: all-zero is a valid initial byte representation for this C packet. Starting from
    // zero explicitly initializes the three tail-padding bytes required by the observed ABI.
    let mut packet: DisplayConfigSetSdrWhiteLevel = unsafe { std::mem::zeroed() };
    packet.header = device_header(
        DISPLAYCONFIG_DEVICE_INFO_SET_SDR_WHITE_LEVEL,
        size_of::<DisplayConfigSetSdrWhiteLevel>(),
        target.adapter,
        target.target_id,
    );
    packet.sdr_white_level = required_white_level;
    packet.final_value = 1;

    // SAFETY: the private packet is zero-padded by Rust's aggregate initialization, has the
    // verified Windows ABI layout, begins with a valid header, and remains alive for the call.
    let result = unsafe { DisplayConfigSetDeviceInfo(&raw const packet.header) };
    if result != 0 {
        return ApplyOutcome::SetFailed(result);
    }

    // Every successful private write is checked exactly once through the documented getter.
    // A mismatch is reported but never retried: a concurrent Settings change remains authoritative.
    verification_outcome(
        required_white_level,
        get_sdr_white_level(target.adapter, target.target_id),
    )
}

pub(crate) fn probe_sdr_white_level() -> ProbeOutcome {
    match locate_hdr_target() {
        TargetLookup::Found(target) => ProbeOutcome::Sample(ProbeSample {
            adapter_low: target.adapter.LowPart,
            adapter_high: target.adapter.HighPart,
            target_id: target.target_id,
            raw_white_level: target.current_white_level,
        }),
        TargetLookup::NoHdrTarget => ProbeOutcome::NoHdrTarget,
        TargetLookup::AmbiguousInternalTargets => ProbeOutcome::AmbiguousInternalTargets,
        TargetLookup::TopologyUnavailable => ProbeOutcome::TopologyUnavailable,
        TargetLookup::InspectionUnavailable => ProbeOutcome::InspectionUnavailable,
    }
}

enum TargetLookup {
    Found(HdrTarget),
    NoHdrTarget,
    AmbiguousInternalTargets,
    TopologyUnavailable,
    InspectionUnavailable,
}

fn verification_outcome(expected: u32, observed: Result<u32, i32>) -> ApplyOutcome {
    match observed {
        Ok(observed) if observed == expected => ApplyOutcome::AppliedAndVerified,
        Ok(observed) => ApplyOutcome::VerificationFailed {
            expected,
            observed: Some(observed),
        },
        Err(_) => ApplyOutcome::VerificationFailed {
            expected,
            observed: None,
        },
    }
}

fn locate_hdr_target() -> TargetLookup {
    let Some(paths) = active_paths() else {
        return TargetLookup::TopologyUnavailable;
    };

    let mut seen = HashSet::new();
    let mut candidates = Vec::new();

    for path in paths {
        let target = path.targetInfo;
        let key = TargetKey::new(target.adapterId, target.id);
        if target.targetAvailable.as_bool()
            && seen.insert(key)
            && is_internal(target.outputTechnology)
        {
            match target_name_is_internal(target.adapterId, target.id) {
                Ok(true) => candidates.push((target.adapterId, target.id)),
                // QueryDisplayConfig already identified this as an embedded/internal output.
                // Contradictory target-name metadata cannot safely prove it external.
                Ok(false) | Err(()) => return TargetLookup::InspectionUnavailable,
            }
        }
    }

    let (adapter, target_id) = match unique_candidate(&candidates) {
        Ok(Some(candidate)) => candidate,
        Ok(None) => return TargetLookup::NoHdrTarget,
        Err(()) => return TargetLookup::AmbiguousInternalTargets,
    };
    match hdr_enabled(adapter, target_id) {
        Ok(true) => {}
        Ok(false) => return TargetLookup::NoHdrTarget,
        Err(()) => return TargetLookup::InspectionUnavailable,
    }
    let Ok(current_white_level) = get_sdr_white_level(adapter, target_id) else {
        return TargetLookup::InspectionUnavailable;
    };
    TargetLookup::Found(HdrTarget {
        adapter,
        target_id,
        current_white_level,
    })
}

fn unique_candidate(candidates: &[(LUID, u32)]) -> Result<Option<(LUID, u32)>, ()> {
    match candidates {
        [] => Ok(None),
        [candidate] => Ok(Some(*candidate)),
        _ => Err(()),
    }
}

fn active_paths() -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
    for _ in 0..MAX_TOPOLOGY_SNAPSHOTS {
        let mut path_count = 0;
        let mut mode_count = 0;
        // SAFETY: both output counts point to initialized writable storage.
        if unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &raw mut path_count, &raw mut mode_count)
        } != ERROR_SUCCESS
        {
            return None;
        }

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        // SAFETY: the arrays have capacities described by their mutable counts. The active-path
        // query does not use a topology-id output.
        let result = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &raw mut path_count,
                paths.as_mut_ptr(),
                &raw mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if result != ERROR_SUCCESS {
            return None;
        }

        paths.truncate(path_count as usize);
        return Some(paths);
    }

    None
}

fn target_name_is_internal(adapter: LUID, target_id: u32) -> Result<bool, ()> {
    let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
            adapter,
            target_id,
        ),
        ..Default::default()
    };
    // SAFETY: the packet has the documented type/size and writable storage for all outputs.
    if unsafe { DisplayConfigGetDeviceInfo(&raw mut name.header) } == 0 {
        Ok(is_internal(name.outputTechnology))
    } else {
        Err(())
    }
}

fn hdr_enabled(adapter: LUID, target_id: u32) -> Result<bool, ()> {
    advanced_color_info_2(adapter, target_id)
        .or_else(|| advanced_color_info_legacy(adapter, target_id))
        .ok_or(())
}

fn get_sdr_white_level(adapter: LUID, target_id: u32) -> Result<u32, i32> {
    let mut white_level = DISPLAYCONFIG_SDR_WHITE_LEVEL {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
            size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(),
            adapter,
            target_id,
        ),
        ..Default::default()
    };
    // SAFETY: the packet has the documented type/size and writable output field.
    let result = unsafe { DisplayConfigGetDeviceInfo(&raw mut white_level.header) };
    if result == 0 {
        Ok(white_level.SDRWhiteLevel)
    } else {
        Err(result)
    }
}

fn advanced_color_info_2(adapter: LUID, target_id: u32) -> Option<bool> {
    let mut info = DisplayConfigGetAdvancedColorInfo2 {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2,
            size_of::<DisplayConfigGetAdvancedColorInfo2>(),
            adapter,
            target_id,
        ),
        ..Default::default()
    };
    // SAFETY: this locally declared packet exactly matches the Windows 11 SDK ABI and remains
    // writable for the call.
    if unsafe { DisplayConfigGetDeviceInfo(&raw mut info.header) } != 0 {
        return None;
    }

    Some(hdr_v2_enabled(info.flags, info.active_color_mode))
}

fn advanced_color_info_legacy(adapter: LUID, target_id: u32) -> Option<bool> {
    let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: device_header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
            adapter,
            target_id,
        ),
        ..Default::default()
    };
    // SAFETY: the packet has the documented type/size and writable output fields.
    if unsafe { DisplayConfigGetDeviceInfo(&raw mut info.header) } != 0 {
        return None;
    }

    // Legacy bit 0 is supported; bit 1 is enabled. A successful SDR-white-level query below
    // further rejects non-HDR advanced-color targets.
    let flags = unsafe { info.Anonymous.value };
    Some(legacy_advanced_color_enabled(flags))
}

const fn hdr_v2_enabled(flags: u32, active_color_mode: i32) -> bool {
    let hdr_supported = flags & (1 << 4) != 0;
    let hdr_user_enabled = flags & (1 << 5) != 0;
    hdr_supported && hdr_user_enabled && active_color_mode == HDR_MODE
}

const fn legacy_advanced_color_enabled(flags: u32) -> bool {
    flags & 0b11 == 0b11
}

fn device_header(
    r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    adapter: LUID,
    target_id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type,
        size: u32::try_from(size).unwrap_or(0),
        adapterId: adapter,
        id: target_id,
    }
}

fn is_internal(technology: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY) -> bool {
    technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
        || technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
        || technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED
        || technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS
}

#[cfg(test)]
mod tests {
    use super::{
        DISPLAYCONFIG_DEVICE_INFO_SET_SDR_WHITE_LEVEL, DisplayConfigGetAdvancedColorInfo2,
        DisplayConfigSetSdrWhiteLevel, hdr_v2_enabled, legacy_advanced_color_enabled,
        unique_candidate, verification_outcome,
    };
    use std::mem::{align_of, offset_of, size_of};
    use windows::Win32::Foundation::LUID;

    #[test]
    fn private_setter_packet_matches_observed_windows_abi() {
        assert_eq!(DISPLAYCONFIG_DEVICE_INFO_SET_SDR_WHITE_LEVEL.0, -18);
        assert_eq!(size_of::<DisplayConfigSetSdrWhiteLevel>(), 28);
        assert_eq!(align_of::<DisplayConfigSetSdrWhiteLevel>(), 4);
        assert_eq!(offset_of!(DisplayConfigSetSdrWhiteLevel, header), 0);
        assert_eq!(
            offset_of!(DisplayConfigSetSdrWhiteLevel, sdr_white_level),
            20
        );
        assert_eq!(offset_of!(DisplayConfigSetSdrWhiteLevel, final_value), 24);
    }

    #[test]
    fn advanced_color_v2_packet_matches_windows_11_sdk_abi() {
        assert_eq!(size_of::<DisplayConfigGetAdvancedColorInfo2>(), 36);
        assert_eq!(align_of::<DisplayConfigGetAdvancedColorInfo2>(), 4);
    }

    #[test]
    fn hdr_v2_requires_support_user_enablement_and_active_hdr_mode() {
        assert!(hdr_v2_enabled((1 << 4) | (1 << 5), 2));
        assert!(!hdr_v2_enabled(1 << 4, 2));
        assert!(!hdr_v2_enabled(1 << 5, 2));
        assert!(!hdr_v2_enabled((1 << 4) | (1 << 5), 0));
        assert!(!hdr_v2_enabled((1 << 4) | (1 << 5), 1));
    }

    #[test]
    fn legacy_gate_requires_supported_and_enabled_bits() {
        assert!(legacy_advanced_color_enabled(0b11));
        assert!(legacy_advanced_color_enabled(0xffff_ffff));
        assert!(!legacy_advanced_color_enabled(0b01));
        assert!(!legacy_advanced_color_enabled(0b10));
    }

    #[test]
    fn target_selection_rejects_ambiguity() {
        let first = (
            LUID {
                LowPart: 1,
                HighPart: 2,
            },
            3,
        );
        let second = (first.0, 4);

        assert!(unique_candidate(&[]).unwrap().is_none());
        assert_eq!(unique_candidate(&[first]).unwrap().unwrap().1, 3);
        assert!(unique_candidate(&[first, second]).is_err());
    }

    #[test]
    fn post_write_verification_requires_exact_public_getter_value() {
        assert!(matches!(
            verification_outcome(3_500, Ok(3_500)),
            super::ApplyOutcome::AppliedAndVerified
        ));
        assert!(matches!(
            verification_outcome(3_500, Ok(3_450)),
            super::ApplyOutcome::VerificationFailed {
                expected: 3_500,
                observed: Some(3_450)
            }
        ));
        assert!(matches!(
            verification_outcome(3_500, Err(5)),
            super::ApplyOutcome::VerificationFailed {
                expected: 3_500,
                observed: None
            }
        ));
    }
}
