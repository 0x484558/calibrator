use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

const CARGO_WIX_MAIN_REV: &str = "fde983c2e901970267e76b8fd68120fdd5457a57";

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_owned());

    match command.as_str() {
        "package-msi" => {
            let mut target = None;
            while let Some(arg) = args.next() {
                if arg == "--target" {
                    target = args.next();
                } else if let Some(t) = arg.strip_prefix("--target=") {
                    target = Some(t.to_owned());
                }
            }
            package_msi(target.as_deref())
        }
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => Err(format!(
            "unknown xtask command `{other}`. Run `cargo xtask help`."
        )),
    }
}

fn package_msi(target: Option<&str>) -> Result<(), String> {
    require_windows_host()?;
    ensure_modern_cargo_wix()?;
    ensure_wix_cli()?;

    let mut build_args = vec!["build", "--release", "--locked", "--package", "calibrator"];
    if let Some(t) = target {
        build_args.push("--target");
        build_args.push(t);
    }

    run_checked(
        Command::new("cargo").args(&build_args),
        &format!("cargo {}", build_args.join(" ")),
    )?;

    let target_bin_dir = match target {
        Some(t) => format!("target\\{t}\\release"),
        None => "target\\release".to_owned(),
    };

    let msi = default_msi_path(target);
    if let Some(parent) = msi.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create `{}`: {error}", parent.display()))?;
    }

    let msi_str = msi.to_string_lossy().to_string();
    let wix_args = vec![
        "wix".to_owned(),
        "--toolset".to_owned(),
        "modern".to_owned(),
        "--migrate".to_owned(),
        "none".to_owned(),
        "--no-build".to_owned(),
        "--target-bin-dir".to_owned(),
        target_bin_dir,
        "--package".to_owned(),
        "calibrator".to_owned(),
        "--nocapture".to_owned(),
        "--output".to_owned(),
        msi_str,
    ];

    run_checked(
        Command::new("cargo").args(&wix_args),
        &format!("cargo {}", wix_args.join(" ")),
    )?;

    println!("MSI written to {}", msi.display());
    Ok(())
}

fn ensure_modern_cargo_wix() -> Result<(), String> {
    let help = output_checked(
        Command::new("cargo").args(["wix", "--help"]),
        "cargo wix --help",
    )?;
    let init_help = output_checked(
        Command::new("cargo").args(["wix", "init", "--help"]),
        "cargo wix init --help",
    )?;

    if help.contains("--toolset <toolset>")
        && help.contains("--migrate <migrate>")
        && init_help.contains("--schema <schema>")
    {
        return Ok(());
    }

    Err(format!(
        "installed cargo-wix does not expose modern WiX support. Install the pinned upstream build with: cargo install --git https://github.com/volks73/cargo-wix --rev {CARGO_WIX_MAIN_REV} cargo-wix --force"
    ))
}

fn ensure_wix_cli() -> Result<(), String> {
    let version = output_checked(Command::new("wix").arg("--version"), "wix --version")?;
    let version = version.trim();
    if version.starts_with('4')
        || version.starts_with('5')
        || version.starts_with('6')
        || version.starts_with('7')
    {
        Ok(())
    } else {
        Err(format!(
            "modern WiX CLI 4 or newer is required; `wix --version` returned `{version}`"
        ))
    }
}

fn default_msi_path(target: Option<&str>) -> PathBuf {
    let filename = match target {
        Some(t) => format!("calibrator-0.1.0-{t}.msi"),
        None => "calibrator-0.1.0-x86_64.msi".to_owned(),
    };
    Path::new("target").join("wix").join(filename)
}

fn require_windows_host() -> Result<(), String> {
    if cfg!(windows) {
        Ok(())
    } else {
        Err("this xtask command must be run on Windows".to_owned())
    }
}

fn run_checked(command: &mut Command, label: &str) -> Result<(), String> {
    let status =
        run_status(command).map_err(|error| format!("failed to run `{label}`: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{label}` failed with status {status}"))
    }
}

fn run_status(command: &mut Command) -> std::io::Result<ExitStatus> {
    command.status()
}

fn output_checked(command: &mut Command, label: &str) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|error| format!("failed to run `{label}`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{label}` failed with status {}:\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn print_help() {
    println!(
        "calibrator xtask\n\nCommands:\n  package-msi [--target <TARGET>]   Build target\\wix\\calibrator-0.1.0-<TARGET>.msi with cargo-wix and modern WiX\n"
    );
    println!(
        "Pinned cargo-wix main install:\n  cargo install --git https://github.com/volks73/cargo-wix --rev {CARGO_WIX_MAIN_REV} cargo-wix --force"
    );
}
