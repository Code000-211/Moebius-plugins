//! The plugin's settings. The core's `[[plugins]]` entry has no table of settings (the core stays generic), so the
//! settings are the entry's `args`, as command-line flags:
//!
//! | Flag | Setting | Default |
//! |---|---|---|
//! | `--nono <program>` | `nono` | `nono` |
//! | `--nono-version <version>` | `nono_version` | [`PINNED_NONO_VERSION`] |
//! | `--network moebius\|internet` | `network` | `moebius` |
//! | `--unix-sockets deny\|allow` | `unix_sockets` | `deny` |
//! | `--read-only <path>` (repeatable) | `read_only` | none |
//! | `--exclude-agent <name>` (repeatable) | `exclude_agents` | none |
//! | `--agent-network <name>=<mode>` (repeatable) | `agents.<name>.network` | none |
//! | `--profiles-dir <dir>` | where the profiles are written | `.moebius-nono-profiles` in the plugin's working directory |

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::PINNED_NONO_VERSION;

/// How far the network reaches from inside the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// Only the router, the gateway and the check address of the request.
    Moebius,
    /// Those and general outbound access.
    Internet,
}

impl Network {
    fn parse(text: &str) -> Result<Self, SettingsError> {
        match text {
            "moebius" => Ok(Self::Moebius),
            "internet" => Ok(Self::Internet),
            other => Err(SettingsError::BadNetwork(other.to_string())),
        }
    }
}

/// Whether a run may connect to the Unix sockets it can reach by path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixSockets {
    /// nono mediates pathname sockets and grants none, so a run reaches no socket of its user.
    Deny,
    /// nono does not mediate them. For WSL2, whose kernel cannot give nono the seccomp listener that mediation needs.
    Allow,
}

impl UnixSockets {
    fn parse(text: &str) -> Result<Self, SettingsError> {
        match text {
            "deny" => Ok(Self::Deny),
            "allow" => Ok(Self::Allow),
            other => Err(SettingsError::BadUnixSockets(other.to_string())),
        }
    }
}

/// Why the arguments are not settings.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("the flag {0} needs a value")]
    MissingValue(String),
    #[error("unknown setting {0}")]
    UnknownFlag(String),
    #[error("the network mode must be \"moebius\" or \"internet\", not {0:?}")]
    BadNetwork(String),
    #[error("--unix-sockets must be \"deny\" or \"allow\", not {0:?}")]
    BadUnixSockets(String),
    #[error("--agent-network takes <agent>=<mode>, not {0:?}")]
    BadAgentNetwork(String),
    #[error("--read-only takes an absolute path without $ or *, not {0:?}")]
    BadReadOnly(String),
}

/// What the plugin was started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub nono: String,
    pub nono_version: String,
    pub network: Network,
    pub unix_sockets: UnixSockets,
    pub read_only: Vec<PathBuf>,
    pub exclude_agents: BTreeSet<String>,
    pub agent_network: BTreeMap<String, Network>,
    pub profiles_dir: PathBuf,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            nono: "nono".into(),
            nono_version: PINNED_NONO_VERSION.into(),
            network: Network::Moebius,
            unix_sockets: UnixSockets::Deny,
            read_only: Vec::new(),
            exclude_agents: BTreeSet::new(),
            agent_network: BTreeMap::new(),
            profiles_dir: default_profiles_dir(),
        }
    }
}

/// `.moebius-nono-profiles` beside the server (the plugin runs in the directory of `moebius.toml`), not in a shared
/// temporary directory where another user or server could meet it. Absolute when the working directory is known.
fn default_profiles_dir() -> PathBuf {
    let name = PathBuf::from(".moebius-nono-profiles");
    std::path::absolute(&name).unwrap_or(name)
}

impl Settings {
    /// Reads the settings from the plugin's arguments (without the program's name).
    pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> Result<Self, SettingsError> {
        let mut settings = Self::default();
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| SettingsError::MissingValue(flag.clone()))
            };
            match flag.as_str() {
                "--nono" => settings.nono = value()?,
                "--nono-version" => settings.nono_version = value()?,
                "--network" => settings.network = Network::parse(&value()?)?,
                "--unix-sockets" => settings.unix_sockets = UnixSockets::parse(&value()?)?,
                "--read-only" => {
                    let path = value()?;
                    if !std::path::Path::new(&path).is_absolute() || path.contains(['$', '*']) {
                        return Err(SettingsError::BadReadOnly(path));
                    }
                    settings.read_only.push(PathBuf::from(path));
                }
                "--exclude-agent" => {
                    settings.exclude_agents.insert(value()?);
                }
                "--agent-network" => {
                    let pair = value()?;
                    let (agent, mode) = pair
                        .split_once('=')
                        .filter(|(agent, _)| !agent.is_empty())
                        .ok_or_else(|| SettingsError::BadAgentNetwork(pair.clone()))?;
                    settings
                        .agent_network
                        .insert(agent.to_string(), Network::parse(mode)?);
                }
                "--profiles-dir" => {
                    let dir = PathBuf::from(value()?);
                    settings.profiles_dir = std::path::absolute(&dir).unwrap_or(dir);
                }
                _ => return Err(SettingsError::UnknownFlag(flag)),
            }
        }
        Ok(settings)
    }

    /// Whether any agent can run in mode `moebius`, which needs the kernel to filter TCP by port.
    pub fn needs_tcp_filtering(&self) -> bool {
        self.network == Network::Moebius
            || self
                .agent_network
                .values()
                .any(|mode| *mode == Network::Moebius)
    }

    /// The network mode of one agent: its own, else the plugin's.
    pub fn network_for(&self, agent: &str) -> Network {
        self.agent_network
            .get(agent)
            .copied()
            .unwrap_or(self.network)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Settings, SettingsError> {
        Settings::from_args(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let settings = parse(&[]).unwrap();
        assert_eq!(settings.nono, "nono");
        assert_eq!(settings.nono_version, PINNED_NONO_VERSION);
        assert_eq!(settings.network, Network::Moebius);
        assert_eq!(settings.unix_sockets, UnixSockets::Deny);
        assert!(settings.read_only.is_empty() && settings.exclude_agents.is_empty());
    }

    // The paths are Unix ones, which Windows does not count as absolute; the plugin refuses to start there anyway.
    #[cfg(unix)]
    #[test]
    fn every_flag_is_read() {
        let settings = parse(&[
            "--nono",
            "/opt/nono",
            "--nono-version",
            "0.82.0",
            "--network",
            "internet",
            "--unix-sockets",
            "allow",
            "--read-only",
            "/opt/tools",
            "--read-only",
            "/srv/sdk",
            "--exclude-agent",
            "Moebius",
            "--agent-network",
            "Coder=moebius",
            "--profiles-dir",
            "/run/profiles",
        ])
        .unwrap();
        assert_eq!(settings.nono, "/opt/nono");
        assert_eq!(settings.nono_version, "0.82.0");
        assert_eq!(settings.network, Network::Internet);
        assert_eq!(settings.unix_sockets, UnixSockets::Allow);
        assert_eq!(settings.read_only.len(), 2);
        assert!(settings.exclude_agents.contains("Moebius"));
        assert_eq!(settings.network_for("Coder"), Network::Moebius);
        assert_eq!(settings.network_for("Other"), Network::Internet);
        assert_eq!(settings.profiles_dir, PathBuf::from("/run/profiles"));
    }

    #[test]
    fn nonsense_is_refused_with_a_sentence() {
        assert_eq!(
            parse(&["--network", "open"]),
            Err(SettingsError::BadNetwork("open".into()))
        );
        assert_eq!(
            parse(&["--unix-sockets", "open"]),
            Err(SettingsError::BadUnixSockets("open".into()))
        );
        assert_eq!(
            parse(&["--nono"]),
            Err(SettingsError::MissingValue("--nono".into()))
        );
        assert_eq!(
            parse(&["--nope"]),
            Err(SettingsError::UnknownFlag("--nope".into()))
        );
        assert!(matches!(
            parse(&["--agent-network", "Coder"]),
            Err(SettingsError::BadAgentNetwork(_))
        ));
        assert!(matches!(
            parse(&["--agent-network", "=internet"]),
            Err(SettingsError::BadAgentNetwork(_))
        ));
        for bad in ["tools", "/opt/$HOME", "/opt/*", ""] {
            assert_eq!(
                parse(&["--read-only", bad]),
                Err(SettingsError::BadReadOnly(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_default_profiles_directory_is_beside_the_server_not_in_the_temp_directory() {
        let dir = parse(&[]).unwrap().profiles_dir;
        assert!(dir.is_absolute() && dir.ends_with(".moebius-nono-profiles"));
        assert!(
            !dir.starts_with(std::env::temp_dir())
                || std::env::current_dir()
                    .unwrap()
                    .starts_with(std::env::temp_dir())
        );
    }

    #[test]
    fn tcp_filtering_is_needed_where_a_mode_is_moebius() {
        assert!(parse(&[]).unwrap().needs_tcp_filtering());
        assert!(
            !parse(&["--network", "internet"])
                .unwrap()
                .needs_tcp_filtering()
        );
        assert!(
            parse(&["--network", "internet", "--agent-network", "Coder=moebius"])
                .unwrap()
                .needs_tcp_filtering()
        );
    }
}
