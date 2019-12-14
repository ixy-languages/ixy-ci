mod config;
mod github;
mod openstack;
mod pcap_tester;
mod publisher;
mod remote;
mod utility;
mod worker;

use std::{fs, io, thread};

use actix_files::Files;
use actix_web::middleware::Logger;
use actix_web::{web, App, HttpServer};
use clap::{crate_version, Arg};
use futures::Stream;
use hubcaps::{Credentials, Github};

use crate::config::Config;
use crate::publisher::Publisher;
use crate::worker::Worker;

fn main() -> io::Result<()> {
    env_logger::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = clap::App::new("ixy-ci server")
        .version(crate_version!())
        .arg(Arg::from_usage("-c, --config <FILE> 'config.toml file'").default_value("config.toml"))
        .get_matches();

    let config = fs::read_to_string(args.value_of("config").unwrap())?;
    let config: Config = toml::from_str(&config).expect("failed to deserialize config");

    let github = Github::new(
        format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        Credentials::Token(config.github.api_token.clone()),
    )
    .expect("failed to initialize GitHub");

    fs::create_dir_all(&config.log_directory).expect("failed to create configured log directory");

    // The OpenStack `Cloud` isn't `Send` so we have to initialize the `Worker` on its own thread
    // and send back some things.
    // TODO: Can we do this more easily?
    let (tx, rx) = std::sync::mpsc::channel();
    let (job_queue_size, log_directory, openstack, test) = (
        config.job_queue_size,
        config.log_directory.clone(),
        config.openstack,
        config.test,
    );
    thread::spawn(move || {
        let (worker, job_sender, report_receiver) =
            Worker::new(job_queue_size, log_directory, openstack, test);

        tx.send((job_sender, report_receiver)).unwrap();

        // TODO: Restart on panic
        worker.run();
    });
    let (job_sender, report_receiver) = rx.recv().unwrap();

    // job_sender
    //     .send(worker::Job::TestBranch {
    //         repository: config::Repository {
    //             user: "ixy-languages".to_string(),
    //             name: "ixy".to_string(),
    //         },
    //         branch: "master".to_string(),
    //     })
    //     .unwrap();

    // job_sender
    //     .send(worker::Job::TestPullRequest {
    //         repository: config::Repository {
    //             user: "bobo1239".to_string(),
    //             name: "ixy.rs".to_string(),
    //         },
    //         pull_request_id: 3,
    //         fork_user: "ixy-languages".to_string(),
    //         fork_branch: "master".to_string(),
    //     })
    //     .unwrap();

    let sys = actix_rt::System::new("runtime");

    let publisher = Publisher::new(github.clone(), config.public_url);
    actix_rt::spawn(
        futures::stream::iter_ok(report_receiver)
            .for_each(move |report| publisher.handle_report(report)),
    );

    let (github_config, log_directory) = (config.github, config.log_directory);
    HttpServer::new(move || {
        App::new()
            .data(github_config.clone())
            .data(job_sender.clone())
            .data(github.clone())
            .wrap(Logger::default())
            .service(Files::new("/logs/", &log_directory))
            .service(web::scope("/github/").service(github::webhook_service))
    })
    .bind(config.bind_address)?
    .start();

    sys.run()
}
