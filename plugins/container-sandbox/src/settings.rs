//! The plugin's settings. The core's `[[plugins]]` entry has no table of settings (the core stays generic), so the
//! settings are the entry's `args`, as command-line flags:
//!
//! | Flag | Setting | Default |
//! |---|---|---|
//! | `--podman <program>` | `podman` | `podman` |
//! | `--image <image>` | `image` | none |
//! | `--repo-image <repository>=<image>` (repeatable) | `repositories.<name>.image` | none |
//! | `--network moebius\|internet` | `network` | `internet` |
//! | `--agent-network <name>=<mode>` (repeatable) | `agents.<name>.network` | none |
//! | `--cache <volume>:<path>` (repeatable) | `caches` | none |
//! | `--exclude-agent <name>` (repeatable) | `exclude_agents` | none |

use std::collections::{BTreeMap, BTreeSet};

/// How far the network reaches from inside the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// Only the ports of the router, the gateway and the check address, on the host's loopback.
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

/// A named Podman volume mounted at a path of the container, kept between runs: a package manager's cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    pub volume: String,
    pub path: String,
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
    #[error("--agent-network takes <agent>=<mode>, not {0:?}")]
    BadAgentNetwork(String),
    #[error("{0:?} is not an image name: it is one word that does not start with -")]
    BadImage(String),
    #[error("--repo-image takes <repository>=<image>, not {0:?}")]
    BadRepoImage(String),
    #[error(
        "--cache takes <volume>:<path>, a volume name of letters, digits, _ . - and an absolute path without : or \
         , in the container, not {0:?}"
    )]
    BadCache(String),
}

/// What the plugin was started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub podman: String,
    pub image: Option<String>,
    pub repo_images: BTreeMap<String, String>,
    pub network: Network,
    pub agent_network: BTreeMap<String, Network>,
    pub caches: Vec<Cache>,
    pub exclude_agents: BTreeSet<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            podman: "podman".into(),
            image: None,
            repo_images: BTreeMap::new(),
            network: Network::Internet,
            agent_network: BTreeMap::new(),
            caches: Vec::new(),
            exclude_agents: BTreeSet::new(),
        }
    }
}

/// An image comes before the container's command in `podman run`, so a value that starts with `-` would be a flag.
fn image(text: String) -> Result<String, SettingsError> {
    if text.is_empty() || text.starts_with('-') || text.contains(char::is_whitespace) {
        return Err(SettingsError::BadImage(text));
    }
    Ok(text)
}

fn cache(text: &str) -> Result<Cache, SettingsError> {
    let bad = || SettingsError::BadCache(text.to_string());
    let (volume, path) = text.split_once(':').ok_or_else(bad)?;
    let volume_ok = volume.starts_with(|c: char| c.is_ascii_alphanumeric())
        && volume
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
    if !volume_ok || !path.starts_with('/') || path.contains([':', ',']) {
        return Err(bad());
    }
    Ok(Cache {
        volume: volume.into(),
        path: path.into(),
    })
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
                "--podman" => settings.podman = value()?,
                "--image" => settings.image = Some(image(value()?)?),
                "--repo-image" => {
                    let pair = value()?;
                    let (repo, name) = pair
                        .split_once('=')
                        .filter(|(repo, _)| !repo.is_empty())
                        .ok_or_else(|| SettingsError::BadRepoImage(pair.clone()))?;
                    settings
                        .repo_images
                        .insert(repo.to_string(), image(name.to_string())?);
                }
                "--network" => settings.network = Network::parse(&value()?)?,
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
                "--cache" => settings.caches.push(cache(&value()?)?),
                "--exclude-agent" => {
                    settings.exclude_agents.insert(value()?);
                }
                _ => return Err(SettingsError::UnknownFlag(flag)),
            }
        }
        Ok(settings)
    }

    /// The network mode of one agent: its own, else the plugin's.
    pub fn network_for(&self, agent: &str) -> Network {
        self.agent_network
            .get(agent)
            .copied()
            .unwrap_or(self.network)
    }

    /// The image of a run of the repository `repository` (`None` for a chat without one): the repository's own,
    /// else the default.
    pub fn image_for(&self, repository: Option<&str>) -> Option<&str> {
        repository
            .and_then(|name| self.repo_images.get(name))
            .or(self.image.as_ref())
            .map(String::as_str)
    }

    /// Every image a run may use, without repeats.
    pub fn images(&self) -> BTreeSet<&str> {
        self.image
            .iter()
            .chain(self.repo_images.values())
            .map(String::as_str)
            .collect()
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
        assert_eq!(settings.podman, "podman");
        assert_eq!(settings.image, None);
        assert_eq!(settings.network, Network::Internet);
        assert!(settings.repo_images.is_empty() && settings.caches.is_empty());
        assert!(settings.exclude_agents.is_empty() && settings.agent_network.is_empty());
    }

    #[test]
    fn every_flag_is_read() {
        let settings = parse(&[
            "--podman",
            "/usr/bin/podman",
            "--image",
            "localhost/moebius-base:1",
            "--repo-image",
            "shop=localhost/shop:latest",
            "--network",
            "internet",
            "--agent-network",
            "Coder=moebius",
            "--cache",
            "moebius-npm:/home/moebius/.npm",
            "--exclude-agent",
            "Moebius",
        ])
        .unwrap();
        assert_eq!(settings.podman, "/usr/bin/podman");
        assert_eq!(settings.image_for(None), Some("localhost/moebius-base:1"));
        assert_eq!(
            settings.image_for(Some("shop")),
            Some("localhost/shop:latest")
        );
        assert_eq!(
            settings.image_for(Some("other")),
            Some("localhost/moebius-base:1")
        );
        assert_eq!(settings.network_for("Coder"), Network::Moebius);
        assert_eq!(settings.network_for("Other"), Network::Internet);
        assert_eq!(
            settings.caches,
            [Cache {
                volume: "moebius-npm".into(),
                path: "/home/moebius/.npm".into()
            }]
        );
        assert!(settings.exclude_agents.contains("Moebius"));
        assert_eq!(
            settings.images().into_iter().collect::<Vec<_>>(),
            ["localhost/moebius-base:1", "localhost/shop:latest"]
        );
    }

    #[test]
    fn without_a_default_image_only_the_named_repositories_have_one() {
        let settings = parse(&["--repo-image", "shop=shop"]).unwrap();
        assert_eq!(settings.image_for(Some("shop")), Some("shop"));
        assert_eq!(settings.image_for(Some("other")), None);
        assert_eq!(settings.image_for(None), None);
    }

    #[test]
    fn nonsense_is_refused_with_a_sentence() {
        assert_eq!(
            parse(&["--network", "open"]),
            Err(SettingsError::BadNetwork("open".into()))
        );
        assert_eq!(
            parse(&["--podman"]),
            Err(SettingsError::MissingValue("--podman".into()))
        );
        assert_eq!(
            parse(&["--nope"]),
            Err(SettingsError::UnknownFlag("--nope".into()))
        );
        for bad in ["Coder", "=internet"] {
            assert!(matches!(
                parse(&["--agent-network", bad]),
                Err(SettingsError::BadAgentNetwork(_))
            ));
        }
        for bad in ["--privileged", "", "two words"] {
            assert_eq!(
                parse(&["--image", bad]),
                Err(SettingsError::BadImage(bad.into())),
                "{bad:?}"
            );
        }
        assert!(matches!(
            parse(&["--repo-image", "shop"]),
            Err(SettingsError::BadRepoImage(_))
        ));
        assert!(matches!(
            parse(&["--repo-image", "shop=--privileged"]),
            Err(SettingsError::BadImage(_))
        ));
        for bad in [
            "npm",
            "npm:relative",
            ":/cache",
            "-v:/cache",
            "a/b:/cache",
            "npm:/a:b",
            "npm:/a,ro",
        ] {
            assert_eq!(
                parse(&["--cache", bad]),
                Err(SettingsError::BadCache(bad.into())),
                "{bad}"
            );
        }
    }
}
