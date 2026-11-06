//! The program Moebius starts from a `[[plugins]]` entry with `launches = true`. Its arguments are the plugin's
//! settings (see `container_sandbox::settings`). It checks the platform, the engine and the images once, then speaks
//! the JSON Lines of the process plugin protocol on its standard input and output. Started with the subcommands `run`,
//! `stop` or `relay`, it is the wrapped launch, its stop command or a Docker container's relay instead.

use std::process::ExitCode;

use container_sandbox::check::{check, resolve};
use container_sandbox::commands::{current_exe, run};
use container_sandbox::container::User;
use container_sandbox::launch::ContainerSandbox;
use container_sandbox::settings::Settings;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(outcome) = run(&args) {
        if !outcome.said.is_empty() {
            eprintln!("{}: {}", container_sandbox::NAME, outcome.said);
        }
        return ExitCode::from(u8::try_from(outcome.code).unwrap_or(1));
    }
    match start(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{}: {why}", container_sandbox::NAME);
            ExitCode::FAILURE
        }
    }
}

fn start(args: Vec<String>) -> Result<(), String> {
    let mut settings = Settings::from_args(args).map_err(|e| e.to_string())?;
    check(&settings, std::env::consts::OS).map_err(|e| e.to_string())?;
    // The wrapped launch and the stop command run in another environment, so name the engine by its path.
    if let Some(path) = resolve(&settings.program) {
        settings.program = path;
    }
    let user = User::current().ok_or("cannot tell the uid and gid of this process")?;
    let engine_env = ContainerSandbox::engine_env_of(std::env::vars());
    let plugin = ContainerSandbox::new(settings, current_exe(), engine_env, user);
    moebius_plugin_api::serve(&plugin, std::io::stdin().lock(), std::io::stdout().lock())
        .map_err(|e| e.to_string())?;
    Ok(())
}
