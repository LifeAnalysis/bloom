#!/usr/bin/env bash

# Recursively list descendants before their parent so cleanup does not lose
# children when an uncooperative debugger exits or reparents them.
bloom_process_tree_pids() {
  local parent="$1" child
  while IFS= read -r child; do
    [ -n "$child" ] || continue
    bloom_process_tree_pids "$child"
  done < <(pgrep -P "$parent" 2>/dev/null || true)
  printf '%s\n' "$parent"
}

bloom_bounded_process() {
  local timeout_secs="$1" output_log="$2"
  shift 2
  case "$timeout_secs" in
    *[!0-9]*|'') return 2 ;;
  esac
  [ "$timeout_secs" -ge 1 ] || return 2

  "$@" >"$output_log" 2>&1 &
  local root_pid=$! deadline
  deadline=$(( $(date +%s) + timeout_secs ))
  while kill -0 "$root_pid" 2>/dev/null; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
      local tracked_pids grace_deadline hard_deadline pid alive
      tracked_pids="$(bloom_process_tree_pids "$root_pid")"
      for pid in $tracked_pids; do
        kill -TERM "$pid" 2>/dev/null || true
      done
      grace_deadline=$(( $(date +%s) + 2 ))
      while [ "$(date +%s)" -lt "$grace_deadline" ]; do
        alive=0
        for pid in $tracked_pids; do
          if kill -0 "$pid" 2>/dev/null; then alive=1; fi
        done
        [ "$alive" -eq 1 ] || break
        sleep 0.05
      done
      # Capture any children created during TERM handling before escalation.
      if kill -0 "$root_pid" 2>/dev/null; then
        tracked_pids="${tracked_pids} $(bloom_process_tree_pids "$root_pid")"
      fi
      for pid in $tracked_pids; do
        kill -KILL "$pid" 2>/dev/null || true
      done
      hard_deadline=$(( $(date +%s) + 2 ))
      while [ "$(date +%s)" -lt "$hard_deadline" ]; do
        kill -0 "$root_pid" 2>/dev/null || break
        sleep 0.05
      done
      # Reap only after kill -0 proves wait cannot block. Never wait past the
      # hard deadline, even if the kernel refuses to terminate the process.
      if ! kill -0 "$root_pid" 2>/dev/null; then
        wait "$root_pid" 2>/dev/null || true
      fi
      printf 'bounded process exceeded %ss\n' "$timeout_secs" >> "$output_log"
      return 124
    fi
    sleep 0.05
  done
  wait "$root_pid"
}
