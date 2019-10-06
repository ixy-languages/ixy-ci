use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt::{self, Display, Formatter};
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Deserialize;

use crate::github;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub bind_address: SocketAddr,
    pub job_queue_size: usize,
    pub log_directory: PathBuf,
    pub github: GitHubConfig,
    pub openstack: OpenStackConfig,
    pub test: TestConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubConfig {
    pub webhook_secrets: HashMap<Repository, String>,
    pub bot_name: String,
    pub api_token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenStackConfig {
    pub flavor: String,
    pub image: String,
    pub keypair: String,
    pub private_key_path: PathBuf,
    pub ssh_login: String,

    // OpenStack API
    pub auth_url: String,
    pub user_name: String,
    pub user_domain: String,
    pub password: String,
    pub project_name: String,
    pub project_domain: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestConfig {
    pub packets: usize,
    pub pci_addresses: PciAddresses,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PciAddresses {
    pub pktgen: String,
    pub fwd_src: String,
    pub fwd_dst: String,
    pub pcap: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    pub build: Vec<String>,
    pub pktgen: String,
    pub fwd: String,
    pub pcap: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(try_from = "String")]
pub struct Repository {
    pub user: String,
    pub name: String,
}

impl TryFrom<String> for Repository {
    type Error = &'static str;
    fn try_from(from: String) -> Result<Repository, &'static str> {
        let mut split = from.split('/');
        let user = split.next().ok_or("missing user")?;
        let name = split.next().ok_or("missing repository name")?;
        match split.next() {
            None => Ok(Repository {
                user: user.to_string(),
                name: name.to_string(),
            }),
            Some(_) => Err("too many \'/\' in repository"),
        }
    }
}

impl From<&github::message::Repository> for Repository {
    fn from(repository: &github::message::Repository) -> Repository {
        Repository {
            user: repository.owner.login.clone(),
            name: repository.name.clone(),
        }
    }
}

impl Display for Repository {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.user, self.name)
    }
}
