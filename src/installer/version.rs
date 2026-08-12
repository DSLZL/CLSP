//! Version parsing shared by all installer candidate validators.

use semver::{Version, VersionReq};

use crate::protocol::ClspError;

pub(super) fn validate_version_output(
    output: &str,
    requirement: &str,
) -> Result<Version, ClspError> {
    let version = parse_version(output).ok_or_else(|| {
        super::server_error(format!(
            "executable version probe returned no semantic version: {output}"
        ))
    })?;
    let requirement = VersionReq::parse(requirement).map_err(super::server_error)?;
    if !requirement.matches(&version) {
        return Err(super::server_error(format!(
            "executable version {version} does not satisfy {requirement}"
        )));
    }
    Ok(version)
}

pub(super) fn parse_version(output: &str) -> Option<Version> {
    output
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+'))
        })
        .filter_map(|candidate| {
            let candidate = candidate
                .strip_prefix("ILS-")
                .or_else(|| candidate.strip_prefix("LS-"))
                .unwrap_or(candidate);
            let candidate = candidate.strip_prefix('v').unwrap_or(candidate);
            Version::parse(candidate)
                .ok()
                .or_else(|| parse_pvp_version(candidate))
                .or_else(|| parse_calendar_version(candidate))
        })
        .next()
}

fn parse_pvp_version(candidate: &str) -> Option<Version> {
    let parse_component = |value: &str| {
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse().ok())
            .flatten()
    };
    let mut components = candidate.split('.');
    let major = parse_component(components.next()?)?;
    let minor = parse_component(components.next()?)?;
    let patch = parse_component(components.next()?)?;
    let _revision: u64 = parse_component(components.next()?)?;
    if components.next().is_some() {
        return None;
    }

    // ponytail: SemVer has three numeric components; keep the PVP revision in the raw probe output.
    Some(Version::new(major, minor, patch))
}

fn parse_calendar_version(candidate: &str) -> Option<Version> {
    let (date, time) = candidate.split_once('-')?;
    let mut date = date.split('.');
    let year = fixed_width_number(date.next()?, 4)?;
    let month = fixed_width_number(date.next()?, 2)?;
    let day = fixed_width_number(date.next()?, 2)?;
    if date.next().is_some() {
        return None;
    }

    let mut time = time.split('.');
    let hour = fixed_width_number(time.next()?, 2)?;
    let minute = fixed_width_number(time.next()?, 2)?;
    let second = fixed_width_number(time.next()?, 2)?;
    if time.next().is_some() || year == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > days_in_month {
        return None;
    }

    // ponytail: compatibility is day-granular; preserve time only if same-day releases diverge.
    Some(Version::new(year, month, day))
}

fn fixed_width_number(value: &str, width: usize) -> Option<u64> {
    (value.len() == width && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}
