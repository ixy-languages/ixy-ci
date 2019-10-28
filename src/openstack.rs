pub use openstack::Error;

use std::net::IpAddr;
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use fallible_iterator::FallibleIterator;
use log::*;
use openstack::auth::Password;
use openstack::network::FloatingIpStatus;
use openstack::{Cloud, ErrorKind, Refresh};
use waiter::Waiter;

use crate::config::OpenStackConfig;
use crate::utility;

// Fixed VM names as we require a specific OpenStack setup anyways
const VM_PKTGEN: &str = "pktgen";
const VM_FWD: &str = "fwd";
const VM_PCAP: &str = "pcap";
const VM_VOLUME_SIZE_GB: u32 = 20;

const RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_RETRIES: usize = 10;

pub struct OpenStack {
    pub config: OpenStackConfig,
    cloud: Cloud,
}

impl OpenStack {
    pub fn new(config: OpenStackConfig) -> Result<OpenStack, Error> {
        let auth = Password::new(
            &config.auth_url,
            &config.user_name,
            &config.password,
            &config.user_domain,
        )?
        .with_project_scope(&config.project_name, &config.project_domain);
        Ok(OpenStack {
            cloud: Cloud::new(auth),
            config,
        })
    }

    pub fn spawn_vms(&self) -> Result<(IpAddr, IpAddr, IpAddr), Error> {
        self.clean_environment()?;

        let ip_pktgen = self.create_server(VM_PKTGEN)?;
        let ip_fwd = self.create_server(VM_FWD)?;
        let ip_pcap = self.create_server(VM_PCAP)?;

        self.add_port_to_vm(VM_PKTGEN, "pktgen")?;
        self.add_port_to_vm(VM_FWD, "fwd-in")?;
        self.add_port_to_vm(VM_FWD, "fwd-out")?;
        self.add_port_to_vm(VM_PCAP, "pcap")?;

        Ok((ip_pktgen, ip_fwd, ip_pcap))
    }

    pub fn clean_environment(&self) -> Result<(), Error> {
        self.delete_server(VM_PKTGEN);
        self.delete_server(VM_FWD);
        self.delete_server(VM_PCAP);

        info!("Deleting unused volumes");
        for v in self.get_unused_volumes()? {
            self.delete_volume(&v)?;
        }

        info!("Deleting unused floating ips");
        self.cloud
            .find_floating_ips()
            .with_status(FloatingIpStatus::Down)
            .into_iter()
            .for_each(|ip| {
                ip.delete()?.wait()?;
                Ok(())
            })
    }

    fn create_server(&self, name: &str) -> Result<IpAddr, Error> {
        info!("Creating server");
        // Port for the internal network must be added later due to some reason I don't understand.
        // We also can't just connect to the network and use an auto-generated port as we need to
        // disable port security (anti-spoofing) which isn't supported yet by the openstack crate.
        let mut server = self
            .cloud
            .new_server(name, &*self.config.flavor)
            .with_new_boot_volume(&*self.config.image, VM_VOLUME_SIZE_GB)
            .with_network("internet")
            .with_keypair(&*self.config.keypair)
            .create()?
            .wait()?;

        let internet_port = self.cloud.find_ports().with_device_id(server.id()).one()?;

        let mut floating_ip = self.cloud.new_floating_ip("internet_pool").create()?;
        info!("Associating floating ip");
        floating_ip.associate(internet_port, None)?;

        // Wait a bit and then retry until the floating ip is fully associated
        thread::sleep(RETRY_DELAY);
        utility::retry(MAX_RETRIES, RETRY_DELAY, || {
            server.refresh()?;
            server
                .floating_ip()
                .ok_or_else(|| Error::new(ErrorKind::OperationTimedOut, "ip association timed out"))
        })
    }

    fn delete_server(&self, name: &str) {
        info!("Deleting server");
        match self.cloud.get_server(name) {
            Ok(server) => {
                server
                    .delete()
                    .expect("can't delete server")
                    .wait()
                    .expect("failed to delete server");
            }
            // TODO: missing != error
            _ => info!("Server doesn't exist or failed to query"),
        }
    }

    fn get_unused_volumes(&self) -> Result<Vec<String>, Error> {
        self.wrap_openstack_cli(
            &[
                "volume",
                "list",
                "-f",
                "value",
                "--status",
                "available",
                "-c",
                "ID",
            ],
            |output| {
                String::from_utf8(output.stdout)
                    .map_err(|_| {
                        Error::new(
                            ErrorKind::InvalidResponse,
                            "openstack cli: failed to parse output",
                        )
                    })
                    .map(|s| s.lines().map(|s| s.to_string()).collect())
            },
        )
    }

    fn delete_volume(&self, id: &str) -> Result<(), Error> {
        self.wrap_openstack_cli(&["volume", "delete", id], |_| Ok(()))
    }

    fn add_port_to_vm(&self, server: &str, port: &str) -> Result<(), Error> {
        // TODO: This fails for some reason...
        // let port = cloud
        //     .find_ports()
        //     .with_name(port)
        //     .with_network("pktgen-fwd")
        //     .one()?;
        // port.with_device_id(server.id()).with_device_owner("compute:nova").with_admin_state_up(true).save().?;

        self.wrap_openstack_cli(&["server", "add", "port", server, port], |_| Ok(()))
    }

    // TODO: Replace usages of the OpenStack CLI once the openstack crate supports everything we need
    fn wrap_openstack_cli<T, F: Fn(Output) -> Result<T, Error>>(
        &self,
        args: &[&str],
        map: F,
    ) -> Result<T, Error> {
        // TODO: Extract config from Cloud and remove dependency on second configuration file
        let output = Command::new("openstack")
            .env("OS_IDENTITY_API_VERSION", "3")
            .env("OS_AUTH_URL", &self.config.auth_url)
            .env("OS_USERNAME", &self.config.user_name)
            .env("OS_USER_DOMAIN_NAME", &self.config.user_domain)
            .env("OS_PASSWORD", &self.config.password)
            .env("OS_PROJECT_NAME", &self.config.project_name)
            .env("OS_PROJECT_DOMAIN_NAME", &self.config.project_domain)
            .args(args)
            .output()
            .expect("failed to execute openstack cli");
        if output.status.success() {
            map(output)
        } else {
            Err(Error::new(
                ErrorKind::InvalidResponse,
                format!("openstack cli failed: {:?}", output),
            ))
        }
    }
}
