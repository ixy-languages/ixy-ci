FROM debian:buster
EXPOSE 8080
VOLUME /config
RUN apt-get update && apt-get --yes install python-openstackclient libssl1.1 ca-certificates && apt-get clean
COPY target/release/ixy-ci /ixy-ci
COPY runner/target/release/runner /runner-bin
COPY run.sh /run.sh
CMD ["/run.sh"]

