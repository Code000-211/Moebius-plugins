//! The program Moebius starts from a `[[plugins]]` entry with `launches = true`. Its arguments are the plugin's
//! settings (see `podman_sandbox::settings`). It checks the platform, Podman and the images once, then speaks the JSON
//! Lines of the process plugin protocol on its standard input and output. Started with the subcommands `run` or
//! `stop`, it is the wrapped launch or its stop command instead.

use std::process::ExitCode;

use podman_sandbox::check::{check, resolve};
use podman_sandbox::commands::{current_exe, run};
use podman_sandbox::launch::PodmanSandbox;
use podman_sandbox::settings::Settings;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(outcome) = run(&args) {
        if !outcome.said.is_empty() {
            eprintln!("podman-sandbox: {}", outcome.said);
        }
        return ExitCode::from(u8::try_from(outcome.code).unwrap_or(1));
    }
    match start(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("podman-sandbox: {why}");
            ExitCode::FAILURE
        }
    }
}

fn start(args: Vec<String>) -> Result<(), String> {
    let mut settings = Settings::from_args(args).map_err(|e| e.to_string())?;
    check(&settings, std::env::consts::OS).map_err(|e| e.to_string())?;
    // The wrapped launch and the stop command run in another environment, so name Podman by its path.
    if let Some(path) = resolve(&settings.podman) {
        settings.podman = path;
    }
    let podman_env = PodmanSandbox::podman_env_of(std::env::vars());
    let plugin = PodmanSandbox::new(settings, current_exe(), podman_env);
    moebius_plugin_api::serve(&plugin, std::io::stdin().lock(), std::io::stdout().lock())
        .map_err(|e| e.to_string())?;
    Ok(())
}
