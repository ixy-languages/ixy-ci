pub use openstack::Error;

use std::net::IpAddr;
use std::process::{Command, Output};
use std::time::Duration;

use fallible_iterator::FallibleIterator;
use log::*;
use openstack::auth::Password;
use openstack::network::FloatingIpStatus;
use openstack::{Cloud, ErrorKind, Refresh};
use waiter::Waiter;

use crate::config::OpenStackConfig;

// Fixed VM names as we require a specific OpenStack setup anyways
const VM_PKTGEN: &str = "pktgen";
const VM_FWD: &str = "fwd";
const VM_PCAP: &str = "pcap";
const VM_VOLUME_SIZE_GB: u32 = 20;

const RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_RETRIES: usize = 10;

pub fn connect_to_cloud(config: &OpenStackConfig) -> Result<Cloud, Error> {
    let auth = Password::new(
        &config.auth_url,
        &config.user_name,
        &config.password,
        &config.user_domain,
    )?
    .with_project_scope(&config.project_name, &config.project_domain);
    Ok(Cloud::new(auth))
}

pub fn spawn_vms(
    cloud: &Cloud,
    config: &OpenStackConfig,
) -> Result<(IpAddr, IpAddr, IpAddr), Error> {
    let flavor = &config.flavor;
    let image = &config.image;
    let keypair = &config.keypair;

    clean_environment(&cloud)?;

    let ip_pktgen = create_server(&cloud, VM_PKTGEN, flavor, image, keypair)?;
    let ip_fwd = create_server(&cloud, VM_FWD, flavor, image, keypair)?;
    let ip_pcap = create_server(&cloud, VM_PCAP, flavor, image, keypair)?;

    add_port_to_vm(VM_PKTGEN, "pktgen")?;
    add_port_to_vm(VM_FWD, "fwd-in")?;
    add_port_to_vm(VM_FWD, "fwd-out")?;
    add_port_to_vm(VM_PCAP, "pcap")?;

    Ok((ip_pktgen, ip_fwd, ip_pcap))
}

pub fn clean_environment(cloud: &Cloud) -> Result<(), Error> {
    delete_server(&cloud, VM_PKTGEN);
    delete_server(&cloud, VM_FWD);
    delete_server(&cloud, VM_PCAP);

    info!("Deleting unused volumes");
    for v in get_unused_volumes()? {
        delete_volume(&v)?;
    }

    info!("Deleting unused floating ips");
    cloud
        .find_floating_ips()
        .with_status(FloatingIpStatus::Down)
        .into_iter()
        .for_each(|ip| {
            ip.delete()?.wait()?;
            Ok(())
        })
}

fn create_server(
    cloud: &Cloud,
    name: &str,
    flavor: &str,
    image: &str,
    keypair: &str,
) -> Result<IpAddr, Error> {
    info!("Creating server");
    // Port for the internal network must be added later due to some reason I don't understand.
    // We also can't just connect to the network and use an auto-generated port as we need to
    // disable port security (anti-spoofing) which isn't supported yet by the openstack crate.
    let mut server = cloud
        .new_server(name, flavor)
        .with_new_boot_volume(image, VM_VOLUME_SIZE_GB)
        .with_network("internet")
        .with_keypair(keypair)
        .create()?
        .wait()?;

    let internet_port = cloud.find_ports().with_device_id(server.id()).one()?;

    let mut floating_ip = cloud.new_floating_ip("internet_pool").create()?;
    info!("Associating floating ip");
    floating_ip.associate(internet_port, None)?;

    // Retry until the floating ip is fully associated
    for _ in 0..MAX_RETRIES {
        match server.floating_ip() {
            None => {
                std::thread::sleep(RETRY_DELAY);
                server.refresh()?;
            }
            Some(ip) => return Ok(ip),
        }
    }
    Err(Error::new(
        ErrorKind::OperationTimedOut,
        "ip association timed out",
    ))
}

fn delete_server(cloud: &Cloud, name: &str) {
    info!("Deleting server");
    match cloud.get_server(name) {
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

fn get_unused_volumes() -> Result<Vec<String>, Error> {
    wrap_openstack_cli(
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

fn delete_volume(id: &str) -> Result<(), Error> {
    wrap_openstack_cli(&["volume", "delete", id], |_| Ok(()))
}

fn add_port_to_vm(server: &str, port: &str) -> Result<(), Error> {
    // TODO: This fails for some reason...
    // let port = cloud
    //     .find_ports()
    //     .with_name(port)
    //     .with_network("pktgen-fwd")
    //     .one()?;
    // port.with_device_id(server.id()).with_device_owner("compute:nova").with_admin_state_up(true).save().?;

    wrap_openstack_cli(&["server", "add", "port", server, port], |_| Ok(()))
}

// TODO: Replace usages of the OpenStack CLI once the openstack crate supports everything we need
fn wrap_openstack_cli<T, F: Fn(Output) -> Result<T, Error>>(
    args: &[&str],
    map: F,
) -> Result<T, Error> {
    // TODO: Extract config from Cloud and remove dependency on second configuration file
    let mut args = args.to_vec();
    args.extend_from_slice(&["--os-cloud", "openstack"]);
    let output = Command::new("openstack")
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
