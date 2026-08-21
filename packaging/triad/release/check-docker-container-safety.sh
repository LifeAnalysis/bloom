#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 CONTAINER" >&2
  exit 64
fi

container=$1
inspect=$(docker inspect "$container" 2>/dev/null) || {
  echo "container does not exist: $container" >&2
  exit 66
}

readarray -t values < <(jq -r '.[0] | [
  (.State.Status // ""),
  ((.HostConfig.Privileged // false) | tostring),
  (.HostConfig.CgroupnsMode // ""),
  (.HostConfig.PidMode // ""),
  (.HostConfig.IpcMode // ""),
  (.HostConfig.NetworkMode // ""),
  ((.Config.Cmd // []) | join(" ")),
  ((.Mounts // []) | map([.Source // "", .Destination // "", (.RW // false | tostring)] | @tsv) | .[])
] | .[]' <<<"$inspect")

status=${values[0]}
privileged=${values[1]}
cgroupns=${values[2]}
pid_mode=${values[3]}
ipc_mode=${values[4]}
network_mode=${values[5]}
command=${values[6]}

fail() {
  echo "unsafe Docker container $container: $*" >&2
  exit 1
}

[[ "$privileged" == "false" ]] || fail "privileged mode is enabled"
[[ "$cgroupns" != "host" ]] || fail "host cgroup namespace is enabled"
[[ -z "$pid_mode" || "$pid_mode" == "private" ]] || fail "host PID namespace is enabled"
[[ -z "$ipc_mode" || "$ipc_mode" == "private" ]] || fail "host IPC namespace is enabled"
[[ "$network_mode" != "host" ]] || fail "host network namespace is enabled"
[[ "$command" != */sbin/init* && "$command" != "systemd"* ]] || fail "systemd PID 1 is not allowed on a desktop host"

for mount in "${values[@]:7}"; do
  IFS=$'\t' read -r source destination rw <<<"$mount"
  if [[ "$source" == "/sys/fs/cgroup" || "$destination" == "/sys/fs/cgroup" ]]; then
    fail "host cgroup filesystem is mounted"
  fi
  if [[ "$destination" == /dev/tty* || "$source" == /dev/tty* ]]; then
    fail "host TTY device is exposed"
  fi
done

echo "Docker container is safe for a desktop host: $container (state=$status)"
