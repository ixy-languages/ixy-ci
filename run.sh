#!/bin/bash
export RUST_BACKTRACE=1
export RUST_LOG=info,ixy_ci=trace
mkdir -p /root/.config/openstack
cp /config/clouds.yaml /root/.config/openstack/clouds.yaml || echo "Failed to copy OpenStack clouds.yaml, add clouds.yaml to config volume"
exec /ixy-ci --config /config/config.toml
