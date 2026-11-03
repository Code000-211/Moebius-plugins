//! The program Moebius starts from a `[[plugins]]` entry with `launches = true`. Its arguments are the plugin's
//! settings (see `nono_sandbox::settings`). It checks the platform and nono once, then speaks the JSON Lines of the
//! process plugin protocol on its standard input and output. Started with the subcommands `stop` or `cleanup`, it is
//! the stop or cleanup command of a launch instead.

use std::process::ExitCode;

use nono_sandbox::check::check;
use nono_sandbox::commands::{current_exe, run};
use nono_sandbox::launch::NonoSandbox;
use nono_sandbox::settings::Settings;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(said) = run(&args) {
        if !said.is_empty() {
            eprintln!("nono-sandbox: {said}");
        }
        return ExitCode::SUCCESS;
    }
    match start(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("nono-sandbox: {why}");
            ExitCode::FAILURE
        }
    }
}

fn start(args: Vec<String>) -> Result<(), String> {
    let mut settings = Settings::from_args(args).map_err(|e| e.to_string())?;
    let kernel = nono_sandbox::check::kernel_release();
    check(
        &settings,
        std::env::consts::OS,
        &kernel,
        nono_sandbox::check::is_wsl2(&kernel),
    )
    .map_err(|e| e.to_string())?;
    // The core starts the wrapped program with the run's environment, so name nono by its path.
    if let Some(path) = nono_sandbox::check::resolve(&settings.nono) {
        settings.nono = path;
    }
    let plugin = NonoSandbox::new(settings, current_exe());
    moebius_plugin_api::serve(&plugin, std::io::stdin().lock(), std::io::stdout().lock())
        .map_err(|e| e.to_string())?;
    Ok(())
}
