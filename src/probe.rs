use std::ffi::OsString;
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::calibration::encode_sdr_white_level;
use crate::display::{ProbeOutcome, ProbeSample};
use crate::win32::{Error, Stage};

const DEFAULT_POSITIONS: &[u8] = &[0, 25, 50, 75, 100];
const LOG_DIRECTORY: &str = "Calibrator";
const LOG_FILE: &str = "hdr-sdr-white-level-probe.csv";

pub(crate) enum RunConfig {
    Adjust,
    Probe(ProbeConfig),
}

pub(crate) struct ProbeConfig {
    positions: Vec<u8>,
}

impl RunConfig {
    pub(crate) fn from_environment() -> Result<Self, Error> {
        parse_arguments(std::env::args_os().skip(1))
            .map_err(|()| Error::condition(Stage::ProbeConfiguration))
    }

    pub(crate) const fn adjustment_enabled(&self) -> bool {
        matches!(self, Self::Adjust)
    }
}

pub(crate) struct ProbeRecorder {
    positions: Vec<u8>,
    next: usize,
    session: u128,
    writer: BufWriter<File>,
}

impl ProbeRecorder {
    pub(crate) fn create(config: ProbeConfig) -> Result<Self, Error> {
        let directory = local_log_directory()?;
        create_dir_all(&directory).map_err(|_| Error::condition(Stage::ProbeLog))?;
        let path = directory.join(LOG_FILE);
        let needs_header = path.metadata().map_or(true, |metadata| metadata.len() == 0);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| Error::condition(Stage::ProbeLog))?;
        let mut writer = BufWriter::with_capacity(4 * 1024, file);
        if needs_header {
            writeln!(
                writer,
                "epoch_ms,session,sequence,slider_position,adapter_low,adapter_high,target_id,raw_sdr_white_level,expected_sdr_white_level,encoding_matches,status"
            )
            .map_err(|_| Error::condition(Stage::ProbeLog))?;
            writer
                .flush()
                .map_err(|_| Error::condition(Stage::ProbeLog))?;
        }

        Ok(Self {
            positions: config.positions,
            next: 0,
            session: epoch_millis(),
            writer,
        })
    }

    pub(crate) fn next_position(&self) -> Option<u8> {
        self.positions.get(self.next).copied()
    }

    pub(crate) fn record(&mut self, outcome: ProbeOutcome) -> Result<(), Error> {
        let Some(position) = self.next_position() else {
            return Ok(());
        };
        let expected = encode_sdr_white_level(position);
        let sequence = self.next + 1;

        match outcome {
            ProbeOutcome::Sample(sample) => {
                self.write_sample(sequence, position, expected, sample)?;
                self.next += 1;
            }
            ProbeOutcome::NoHdrTarget => {
                self.write_unavailable(sequence, position, expected, "no_hdr_internal_target")?;
            }
            ProbeOutcome::AmbiguousInternalTargets => {
                self.write_unavailable(sequence, position, expected, "ambiguous_internal_targets")?;
            }
            ProbeOutcome::TopologyUnavailable => {
                self.write_unavailable(sequence, position, expected, "topology_unavailable")?;
            }
            ProbeOutcome::InspectionUnavailable => {
                self.write_unavailable(sequence, position, expected, "inspection_unavailable")?;
            }
        }

        // Captures occur only on explicit tray interaction. Flush each complete sparse record so
        // a completed manual measurement survives an abnormal later termination.
        self.writer
            .flush()
            .map_err(|_| Error::condition(Stage::ProbeLog))
    }

    pub(crate) fn finish(&mut self) -> Result<(), Error> {
        self.writer
            .flush()
            .map_err(|_| Error::condition(Stage::ProbeLog))
    }

    fn write_sample(
        &mut self,
        sequence: usize,
        position: u8,
        expected: u32,
        sample: ProbeSample,
    ) -> Result<(), Error> {
        let matches = sample.raw_white_level == expected;
        writeln!(
            self.writer,
            "{}",
            sample_line(
                epoch_millis(),
                self.session,
                sequence,
                position,
                expected,
                sample,
                matches
            )
        )
        .map_err(|_| Error::condition(Stage::ProbeLog))
    }

    fn write_unavailable(
        &mut self,
        sequence: usize,
        position: u8,
        expected: u32,
        status: &str,
    ) -> Result<(), Error> {
        writeln!(
            self.writer,
            "{}",
            unavailable_line(
                epoch_millis(),
                self.session,
                sequence,
                position,
                expected,
                status
            )
        )
        .map_err(|_| Error::condition(Stage::ProbeLog))
    }
}

fn sample_line(
    epoch: u128,
    session: u128,
    sequence: usize,
    position: u8,
    expected: u32,
    sample: ProbeSample,
    matches: bool,
) -> String {
    format!(
        "{epoch},{session},{sequence},{position},{},{},{},{},{expected},{matches},sample",
        sample.adapter_low, sample.adapter_high, sample.target_id, sample.raw_white_level
    )
}

fn unavailable_line(
    epoch: u128,
    session: u128,
    sequence: usize,
    position: u8,
    expected: u32,
    status: &str,
) -> String {
    format!("{epoch},{session},{sequence},{position},,,,,{expected},false,{status}")
}

fn local_log_directory() -> Result<PathBuf, Error> {
    let base = std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::condition(Stage::ProbeLog))?;
    Ok(PathBuf::from(base).join(LOG_DIRECTORY))
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<RunConfig, ()> {
    let mut probe_positions = None;
    for argument in arguments {
        let argument = argument.to_str().ok_or(())?;
        let positions = if argument == "--probe" {
            DEFAULT_POSITIONS.to_vec()
        } else if let Some(values) = argument.strip_prefix("--probe=") {
            parse_positions(values)?
        } else {
            return Err(());
        };
        if probe_positions.replace(positions).is_some() {
            return Err(());
        }
    }

    Ok(match probe_positions {
        Some(positions) => RunConfig::Probe(ProbeConfig { positions }),
        None => RunConfig::Adjust,
    })
}

fn parse_positions(values: &str) -> Result<Vec<u8>, ()> {
    let positions: Vec<u8> = values
        .split(',')
        .map(|value| value.parse::<u8>().map_err(|_| ()))
        .collect::<Result<_, _>>()?;
    if positions.len() < 2
        || positions.iter().any(|position| *position > 100)
        || positions.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(());
    }
    Ok(positions)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use crate::display::ProbeSample;

    use super::{
        DEFAULT_POSITIONS, RunConfig, parse_arguments, parse_positions, sample_line,
        unavailable_line,
    };

    #[test]
    fn no_arguments_selects_adjustment_mode() {
        assert!(matches!(
            parse_arguments(Vec::<OsString>::new()).unwrap(),
            RunConfig::Adjust
        ));
    }

    #[test]
    fn probe_defaults_cover_endpoints_and_intermediate_positions() {
        let config = parse_arguments([OsString::from("--probe")]).unwrap();
        let RunConfig::Probe(config) = config else {
            panic!("probe mode expected");
        };
        assert_eq!(config.positions, DEFAULT_POSITIONS);
    }

    #[test]
    fn explicit_probe_positions_are_strictly_increasing_and_bounded() {
        assert_eq!(parse_positions("0,10,55,100").unwrap(), [0, 10, 55, 100]);
        assert!(parse_positions("0").is_err());
        assert!(parse_positions("0,101").is_err());
        assert!(parse_positions("0,50,50").is_err());
        assert!(parse_positions("50,0").is_err());
        assert!(parse_positions("0,,100").is_err());
    }

    #[test]
    fn unknown_or_duplicate_modes_are_rejected() {
        assert!(parse_arguments([OsString::from("--other")]).is_err());
        assert!(
            parse_arguments([OsString::from("--probe"), OsString::from("--probe=0,100")]).is_err()
        );
    }

    #[test]
    fn probe_csv_rows_preserve_schema_and_raw_encoding_evidence() {
        let sample = sample_line(
            10,
            11,
            2,
            50,
            3_500,
            ProbeSample {
                adapter_low: 12,
                adapter_high: -1,
                target_id: 13,
                raw_white_level: 3_450,
            },
            false,
        );
        let fields: Vec<_> = sample.split(',').collect();
        assert_eq!(fields.len(), 11);
        assert_eq!(
            fields,
            [
                "10", "11", "2", "50", "12", "-1", "13", "3450", "3500", "false", "sample"
            ]
        );

        let unavailable = unavailable_line(10, 11, 2, 50, 3_500, "inspection_unavailable");
        let fields: Vec<_> = unavailable.split(',').collect();
        assert_eq!(fields.len(), 11);
        assert_eq!(&fields[4..8], &["", "", "", ""]);
        assert_eq!(&fields[8..], &["3500", "false", "inspection_unavailable"]);
    }
}
